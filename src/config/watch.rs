//! Hot-reload support for the config file.
//!
//! [`ConfigWatcher`] watches the config path with [`notify`] and pushes a freshly
//! parsed [`Config`] through a channel whenever the file changes. The daemon
//! swaps its in-memory config without restarting. A parse error during reload is
//! logged and the previous config is kept, so a typo never kills the daemon.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};

use crate::config::Config;
use crate::error::{Error, Result};

/// Watches a config file and yields reloaded [`Config`] values.
pub struct ConfigWatcher {
    path: PathBuf,
    // Kept alive for the lifetime of the watcher; dropping it stops the thread.
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<()>,
}

impl ConfigWatcher {
    /// Start watching `path`. The parent directory is watched (not the file
    /// itself) because editors typically replace the file via rename, which a
    /// file-level watch would miss.
    pub fn new(path: &Path) -> Result<Self> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if event.kind.is_modify() || event.kind.is_create() {
                    // Best-effort: a closed receiver just means we're shutting down.
                    let _ = tx.send(());
                }
            }
        })
        .map_err(|e| Error::Config(format!("failed to create watcher: {e}")))?;

        let watch_dir = path.parent().unwrap_or_else(|| Path::new("."));
        // A brand-new user has no config file *or* config directory yet; without
        // the directory, `notify` has nothing to watch and hot-reload would
        // silently never arm. Create mole's own config dir (best-effort) so the
        // watch attaches and a later first-time save is picked up live.
        if !watch_dir.as_os_str().is_empty() && !watch_dir.exists() {
            let _ = std::fs::create_dir_all(watch_dir);
        }
        watcher
            .watch(watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| Error::Config(format!("failed to watch {watch_dir:?}: {e}")))?;

        Ok(ConfigWatcher {
            path: path.to_path_buf(),
            _watcher: watcher,
            rx,
        })
    }

    /// Block until the config changes, then return the reparsed config.
    ///
    /// Returns `Ok(None)` when the file changed but failed to parse (the caller
    /// should keep its current config), and `Err` only if the watch channel is
    /// gone.
    pub fn next_reload(&self) -> Result<Option<Config>> {
        // Wait for the first event, then debounce: editors often emit a flurry
        // of events for a single save, so we drain anything that arrives within
        // a short window before reading the file once.
        self.rx
            .recv()
            .map_err(|_| Error::Config("config watch channel closed".into()))?;
        while self.rx.recv_timeout(Duration::from_millis(120)).is_ok() {}

        match Config::load(&self.path) {
            Ok(cfg) => {
                log::info!("reloaded config from {:?}", self.path);
                Ok(Some(cfg))
            }
            Err(e) => {
                log::warn!("config reload failed, keeping previous config: {e}");
                Ok(None)
            }
        }
    }
}
