//! Background OCR pre-warming.
//!
//! On its own X connection, this thread watches the whole screen for changes via
//! the X DAMAGE extension and, once the screen settles, re-reads the strips that
//! changed (through the shared [`ScanCache`]). The effect: by the time the user
//! triggers a hint, the cache is already current, so the on-demand scan finds
//! nothing to re-read and the hints appear almost instantly.
//!
//! It deliberately does nothing while an interaction is active — otherwise it
//! would capture mole's own overlay and cache the hint labels as if they were
//! screen text. DAMAGE reports only drawing, not pointer motion (the cursor is a
//! hardware sprite), so ordinary mouse movement never wakes it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection as _;
use x11rb::protocol::damage::{ConnectionExt as _, ReportLevel};
use x11rb::protocol::Event;

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, ScanCache};
use crate::error::{Error, Result};
use crate::x11::Conn;

/// Quiet period the screen must hold before we warm, so we don't OCR every frame
/// of an animation or scroll.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Cap on how long a continuously-changing screen delays a warm, so video or a
/// blinking cursor can't postpone it forever.
const MAX_WAIT: Duration = Duration::from_millis(1200);

/// Spawn the pre-warm thread. `active` is set by the daemon while an interaction
/// owns the screen, telling the warmer to hold off. Failures (no X, no DAMAGE)
/// disable pre-warming with a warning rather than taking the daemon down.
pub fn spawn(config: Arc<Mutex<Config>>, cache: Arc<Mutex<ScanCache>>, active: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        if let Err(e) = run(config, cache, active) {
            log::warn!("OCR pre-warm disabled: {e}");
        }
    });
}

fn run(
    config: Arc<Mutex<Config>>,
    cache: Arc<Mutex<ScanCache>>,
    active: Arc<AtomicBool>,
) -> Result<()> {
    let conn = Conn::open()?;
    conn.conn
        .damage_query_version(1, 1)
        .map_err(Error::x11)?
        .reply()
        .map_err(Error::x11)?;
    let damage = conn.conn.generate_id().map_err(Error::x11)?;
    conn.conn
        .damage_create(damage, conn.root, ReportLevel::NON_EMPTY)
        .map_err(Error::x11)?;
    conn.flush()?;
    log::info!("OCR pre-warm watching the screen");

    // Warm once up front so even the first hint is ready.
    warm(&conn, &config, &cache, &active);

    loop {
        // Block until something on screen changes.
        match conn.conn.wait_for_event().map_err(Error::x11)? {
            Event::DamageNotify(_) => {}
            _ => continue,
        }
        acknowledge(&conn, damage)?;
        wait_until_settled(&conn, damage)?;
        warm(&conn, &config, &cache, &active);
    }
}

/// Drain pending damage until the screen has been quiet for [`DEBOUNCE`], or
/// [`MAX_WAIT`] elapses (so constant motion still lets a warm through).
fn wait_until_settled(conn: &Conn, damage: u32) -> Result<()> {
    let start = Instant::now();
    let mut last_change = Instant::now();
    while last_change.elapsed() < DEBOUNCE && start.elapsed() < MAX_WAIT {
        let mut saw_change = false;
        while let Some(event) = conn.conn.poll_for_event().map_err(Error::x11)? {
            if let Event::DamageNotify(_) = event {
                saw_change = true;
            }
        }
        if saw_change {
            acknowledge(conn, damage)?;
            last_change = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}

/// Reset the damage region so future changes notify again.
fn acknowledge(conn: &Conn, damage: u32) -> Result<()> {
    conn.conn
        .damage_subtract(damage, x11rb::NONE, x11rb::NONE)
        .map_err(Error::x11)?;
    conn.flush()
}

/// Capture the screen and refresh the cache, unless an interaction is active
/// (in which case the capture would contain our own overlay).
fn warm(conn: &Conn, config: &Mutex<Config>, cache: &Arc<Mutex<ScanCache>>, active: &AtomicBool) {
    if active.load(Ordering::SeqCst) {
        return;
    }
    let cfg = config.lock().unwrap().clone();
    if !cfg.ocr.prewarm {
        return;
    }
    let screen = match Screen::capture_full(conn) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("pre-warm capture failed: {e}");
            return;
        }
    };
    let detector = detect::from_config(&cfg, Arc::clone(cache));
    match detector.detect(&screen) {
        Ok(elements) => log::debug!("pre-warm cached {} elements", elements.len()),
        Err(e) => log::warn!("pre-warm OCR failed: {e}"),
    }
}
