//! The background daemon (Plan §5.2).
//!
//! Running mole as a daemon keeps latency low: the X connection, keysym map
//! and detectors are all set up once, so a trigger only has to capture, detect
//! and draw. Triggers arrive over a Unix socket — the natural fit for i3's
//! `bindsym ... exec mole click`.
//!
//! Config is hot-reloaded on a background thread (Plan §5.1); interactions run
//! one at a time on the main thread because they own the keyboard/overlay.

pub mod ipc;
mod prewarm;

pub use ipc::Command;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::{Config, ConfigWatcher};
use crate::detect::{Cancel, ScanCache};
use crate::error::{Error, Result};
use crate::session::Session;
use crate::x11::Conn;

/// The long-running daemon.
pub struct Daemon {
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    socket_path: PathBuf,
    /// OCR cache shared across hints and the background pre-warm, so a hint on a
    /// mostly-unchanged screen re-reads only what moved.
    scan_cache: Arc<Mutex<ScanCache>>,
    /// Set while a hint/move interaction owns the screen, so the pre-warmer holds
    /// off (its capture would otherwise contain our own overlay).
    interaction_active: Arc<AtomicBool>,
    /// Aborts the pre-warm's in-flight OCR when a hint starts, so the hint never
    /// waits out a background read for the shared cache lock.
    prewarm_cancel: Cancel,
}

impl Daemon {
    /// Build a daemon from an initial config and the path it was loaded from.
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        let socket_path = ipc::socket_path(&config);
        Daemon {
            config: Arc::new(Mutex::new(config)),
            config_path,
            socket_path,
            scan_cache: Arc::new(Mutex::new(ScanCache::new())),
            interaction_active: Arc::new(AtomicBool::new(false)),
            prewarm_cancel: Cancel::new(),
        }
    }

    /// Bind the socket, start the config watcher, and serve until killed.
    pub fn run(self) -> Result<()> {
        let conn = Conn::open()?;
        let listener = self.bind_socket()?;
        log::info!("daemon listening on {:?}", self.socket_path);

        self.spawn_config_watcher();
        prewarm::spawn(
            Arc::clone(&self.config),
            Arc::clone(&self.scan_cache),
            Arc::clone(&self.interaction_active),
            self.prewarm_cancel.clone(),
        );

        loop {
            let (stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) => {
                    log::warn!("accept failed: {e}");
                    continue;
                }
            };
            if let Err(e) = self.handle_client(&conn, stream) {
                log::warn!("client handling error: {e}");
            }
            // Triggers spammed while we were busy piled up in the socket backlog;
            // a hint runs one at a time, so discard them rather than replay each
            // as another overlay.
            self.drain_pending(&listener);
        }
    }

    /// Reject every connection currently queued in the backlog. Called right
    /// after an interaction, so a burst of triggers sent while it was on screen
    /// doesn't pop the overlay back up once per queued trigger.
    fn drain_pending(&self, listener: &UnixListener) {
        if listener.set_nonblocking(true).is_err() {
            return;
        }
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = writeln!(stream, "busy: a mole interaction was already in progress");
        }
        let _ = listener.set_nonblocking(false);
    }

    /// Create the listening socket, clearing any stale one left by a crash.
    fn bind_socket(&self) -> Result<UnixListener> {
        if self.socket_path.exists() {
            // If nobody is listening, the old socket is stale; remove it.
            if UnixStream::connect(&self.socket_path).is_err() {
                let _ = std::fs::remove_file(&self.socket_path);
            } else {
                return Err(Error::Ipc(format!(
                    "another daemon is already listening on {:?}",
                    self.socket_path
                )));
            }
        }
        UnixListener::bind(&self.socket_path)
            .map_err(|e| Error::Ipc(format!("could not bind {:?}: {e}", self.socket_path)))
    }

    /// Watch the config file and swap the shared config on change.
    fn spawn_config_watcher(&self) {
        let path = self.config_path.clone();
        let shared = Arc::clone(&self.config);
        let cache = Arc::clone(&self.scan_cache);
        std::thread::spawn(move || {
            let watcher = match ConfigWatcher::new(&path) {
                Ok(w) => w,
                Err(e) => {
                    log::warn!("config hot-reload disabled: {e}");
                    return;
                }
            };
            loop {
                match watcher.next_reload() {
                    Ok(Some(cfg)) => {
                        *shared.lock().unwrap() = cfg;
                        // OCR params may have changed; force a fresh read.
                        cache.lock().unwrap().invalidate();
                    }
                    Ok(None) => {} // parse error already logged; keep current
                    Err(e) => {
                        log::warn!("config watcher stopped: {e}");
                        return;
                    }
                }
            }
        });
    }

    /// Read one command from a client, run it, and reply with a status line.
    fn handle_client(&self, conn: &Conn, stream: UnixStream) -> Result<()> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(Error::Io)?;
        let mut writer = reader.into_inner();

        let response = match Command::parse(&line) {
            Ok(cmd) => match self.dispatch(conn, cmd) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("err: {e}"),
            },
            Err(e) => format!("err: {e}"),
        };
        writeln!(writer, "{response}").map_err(Error::Io)?;
        Ok(())
    }

    /// Execute a single command against a fresh [`Session`] using the current
    /// config snapshot.
    fn dispatch(&self, conn: &Conn, cmd: Command) -> Result<()> {
        if let Command::Ping = cmd {
            return Ok(());
        }
        if let Command::Reload = cmd {
            let cfg = Config::load(&self.config_path)?;
            *self.config.lock().unwrap() = cfg;
            // OCR params may have changed; drop cached words so they're re-read.
            self.scan_cache.lock().unwrap().invalidate();
            log::info!("config reloaded on request");
            return Ok(());
        }

        // Snapshot the config so a mid-interaction reload can't change it.
        let cfg = self.config.lock().unwrap().clone();
        let session = Session::new(conn, &cfg, Arc::clone(&self.scan_cache))?;

        // Tell the pre-warmer to hold off while we own the screen, so it doesn't
        // capture (and cache) our own overlay, and kill any read it has in flight
        // so this hint never waits out a background OCR for the cache lock.
        self.interaction_active.store(true, Ordering::SeqCst);
        self.prewarm_cancel.abort();
        let result = match cmd {
            Command::Hint(mode) => session.run_hint(mode),
            Command::FreeMove => session.run_free_move(),
            Command::Reload | Command::Ping => unreachable!("handled above"),
        };
        // Clear the abort before re-enabling pre-warm so its next read runs.
        self.prewarm_cancel.reset();
        self.interaction_active.store(false, Ordering::SeqCst);
        result
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Best-effort cleanup so the next start doesn't see a stale socket.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
