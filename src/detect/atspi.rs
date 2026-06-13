//! AT-SPI detection over D-Bus (Plan §3.1) — the primary backend.
//!
//! GTK, Qt and Firefox each publish an accessibility tree on the a11y D-Bus.
//! Walking it gives precise rectangles, roles and text in a few milliseconds,
//! with no image analysis. The flow is:
//!
//! 1. Ask the session bus for the a11y bus address (`org.a11y.Bus.GetAddress`).
//! 2. Connect to that bus.
//! 3. From the registry root, recurse the tree, reading each node's role, name
//!    and on-screen extents.
//!
//! Everything is best-effort: a node that errors (interface not implemented,
//! app quitting) is skipped, never fatal. Traversal is bounded in depth and node
//! count so a pathological tree can't stall a hint session.

use zbus::blocking::Connection;
use zvariant::{OwnedObjectPath, Value};

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{Detector, Element, Role, Source};
use crate::error::{Error, Result};
use crate::geometry::Rect;

const ACCESSIBLE_IFACE: &str = "org.a11y.atspi.Accessible";
const COMPONENT_IFACE: &str = "org.a11y.atspi.Component";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const REGISTRY_DEST: &str = "org.a11y.atspi.Registry";
const REGISTRY_ROOT: &str = "/org/a11y/atspi/accessible/root";

/// AT-SPI screen coordinate space.
const COORD_SCREEN: u32 = 0;

// Safety bounds on traversal.
const MAX_NODES: usize = 6000;
const MAX_DEPTH: u32 = 60;

/// A reference to a node in the tree: the bus name that owns it and its path.
#[derive(Debug, Clone)]
struct NodeRef {
    dest: String,
    path: String,
    depth: u32,
}

/// Detector backed by the AT-SPI accessibility tree.
pub struct AtSpiDetector {
    // No live connection is held: each detect() opens a fresh one so the daemon
    // starts even when the a11y bus isn't up yet.
    _private: (),
}

impl AtSpiDetector {
    pub fn new(_config: &Config) -> Result<Self> {
        Ok(AtSpiDetector { _private: () })
    }
}

impl Detector for AtSpiDetector {
    fn name(&self) -> &'static str {
        "atspi"
    }

    fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let client = AtSpiClient::connect()?;
        let mut elements = Vec::new();
        client.walk(screen.bounds(), &mut elements);
        Ok(elements)
    }
}

/// Holds the connection to the a11y bus and the traversal logic.
struct AtSpiClient {
    conn: Connection,
}

impl AtSpiClient {
    /// Resolve the a11y bus address and connect to it.
    fn connect() -> Result<AtSpiClient> {
        let session = Connection::session().map_err(Error::atspi)?;
        let reply = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .map_err(Error::atspi)?;
        let address: String = reply.body().deserialize().map_err(Error::atspi)?;

        let conn = zbus::blocking::connection::Builder::address(address.as_str())
            .map_err(Error::atspi)?
            .build()
            .map_err(Error::atspi)?;
        Ok(AtSpiClient { conn })
    }

    /// Depth-first traversal from the registry root, collecting on-screen,
    /// hintable nodes into `out`.
    fn walk(&self, screen: Rect, out: &mut Vec<Element>) {
        let mut stack = vec![NodeRef {
            dest: REGISTRY_DEST.to_string(),
            path: REGISTRY_ROOT.to_string(),
            depth: 0,
        }];
        let mut visited = 0usize;

        while let Some(node) = stack.pop() {
            if visited >= MAX_NODES {
                log::debug!("atspi: hit node cap ({MAX_NODES})");
                break;
            }
            visited += 1;

            // Visit this node (may or may not yield an element).
            if let Some(el) = self.node_element(&node, screen) {
                out.push(el);
            }

            if node.depth < MAX_DEPTH {
                for child in self.children(&node) {
                    stack.push(NodeRef {
                        dest: child.0,
                        path: child.1.as_str().to_string(),
                        depth: node.depth + 1,
                    });
                }
            }
        }
    }

    /// Turn a node into an [`Element`] if it has on-screen extents and is worth
    /// hinting. Returns `None` for containers, off-screen nodes, or on error.
    fn node_element(&self, node: &NodeRef, screen: Rect) -> Option<Element> {
        let rect = self.extents(node)?;
        // Must overlap the screen and have a positive size.
        if rect.width <= 0 || rect.height <= 0 || rect.clamp_to(screen).is_none() {
            return None;
        }
        let role = Role::from_atspi(self.role(node).unwrap_or(0));
        let text = self.name(node).unwrap_or_default();

        // Offer interactive roles, and any node that carries text.
        if role.is_interactive() && (role != Role::Other || !text.trim().is_empty()) {
            Some(Element::new(rect, text, role, Source::AtSpi))
        } else {
            None
        }
    }

    fn role(&self, node: &NodeRef) -> Option<u32> {
        let reply = self
            .conn
            .call_method(
                Some(node.dest.as_str()),
                node.path.as_str(),
                Some(ACCESSIBLE_IFACE),
                "GetRole",
                &(),
            )
            .ok()?;
        reply.body().deserialize::<u32>().ok()
    }

    fn name(&self, node: &NodeRef) -> Option<String> {
        let reply = self
            .conn
            .call_method(
                Some(node.dest.as_str()),
                node.path.as_str(),
                Some(PROPS_IFACE),
                "Get",
                &(ACCESSIBLE_IFACE, "Name"),
            )
            .ok()?;
        // The property comes back wrapped in a variant.
        let body = reply.body();
        let value: Value = body.deserialize().ok()?;
        match value {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }
    }

    fn extents(&self, node: &NodeRef) -> Option<Rect> {
        let reply = self
            .conn
            .call_method(
                Some(node.dest.as_str()),
                node.path.as_str(),
                Some(COMPONENT_IFACE),
                "GetExtents",
                &(COORD_SCREEN,),
            )
            .ok()?;
        let (x, y, w, h): (i32, i32, i32, i32) = reply.body().deserialize().ok()?;
        Some(Rect::new(x, y, w, h))
    }

    fn children(&self, node: &NodeRef) -> Vec<(String, OwnedObjectPath)> {
        let reply = match self.conn.call_method(
            Some(node.dest.as_str()),
            node.path.as_str(),
            Some(ACCESSIBLE_IFACE),
            "GetChildren",
            &(),
        ) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        reply
            .body()
            .deserialize::<Vec<(String, OwnedObjectPath)>>()
            .unwrap_or_default()
    }
}
