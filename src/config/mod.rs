//! Configuration: TOML on disk, [`serde`] in memory, hot-reloaded by the daemon.
//!
//! Every section has a sensible default so a brand-new user can run mole
//! with no config file at all. [`Config::load`] merges whatever the user wrote
//! on top of those defaults.

mod watch;

pub use watch::ConfigWatcher;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// An RGBA colour, each channel `0..=255`. Serialised as a 4-element array so
/// the TOML reads `background = [255, 220, 90, 230]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba([r, g, b, a])
    }

    /// Channels as cairo-friendly `0.0..=1.0` floats `(r, g, b, a)`.
    pub fn as_f64(&self) -> (f64, f64, f64, f64) {
        let [r, g, b, a] = self.0;
        (
            r as f64 / 255.0,
            g as f64 / 255.0,
            b as f64 / 255.0,
            a as f64 / 255.0,
        )
    }

    /// Perceived luminance in `0.0..=1.0` (Rec. 601 weights). Used to pick a
    /// contrasting text colour over a detected background.
    pub fn luminance(&self) -> f64 {
        let [r, g, b, _] = self.0;
        (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64) / 255.0
    }
}

/// The complete, validated configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keys: Keys,
    pub movement: Movement,
    pub hints: Hints,
    pub ocr: Ocr,
    pub daemon: Daemon,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub hint_alphabet: String,
    pub move_left: String,
    pub move_down: String,
    pub move_up: String,
    pub move_right: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Movement {
    pub step: i32,
    pub large_step: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hints {
    pub background: Rgba,
    pub foreground: Rgba,
    pub foreground_dark: Rgba,
    pub matched: Rgba,
    pub font_family: String,
    pub font_size: f64,
    pub min_gap: i32,
}

/// OCR backend tuning. These are the "sensitivity" knobs for how aggressively
/// text is read off the screen and how words are grouped into phrase targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ocr {
    /// Path/name of the `tesseract` binary.
    pub binary: String,
    /// Recognition language passed to Tesseract (`-l`), e.g. `"eng"`.
    pub language: String,
    /// Minimum Tesseract confidence (`0..=100`) for a word to be kept. Raise it
    /// to drop shaky guesses, lower it to catch faint text.
    pub min_confidence: f32,
    /// Ignore phrase boxes smaller than this many pixels on either axis (noise).
    pub min_element_size: i32,
    /// How readily neighbouring words merge into one phrase target: the maximum
    /// horizontal gap between two words, as a multiple of their text height.
    /// Higher = longer phrases / fewer hints; lower = more, shorter targets.
    pub max_word_gap: f64,
    /// How strict "same line" is: two words share a line when their vertical
    /// centres differ by at most this fraction of their text height.
    pub line_tolerance: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    /// Explicit socket path; when `None` the runtime default is used.
    pub socket: Option<PathBuf>,
}

// --- Defaults -------------------------------------------------------------

impl Default for Keys {
    fn default() -> Self {
        Keys {
            hint_alphabet: "asdfghjkl".to_string(),
            move_left: "h".to_string(),
            move_down: "j".to_string(),
            move_up: "k".to_string(),
            move_right: "l".to_string(),
        }
    }
}

impl Default for Movement {
    fn default() -> Self {
        Movement {
            step: 24,
            large_step: 160,
        }
    }
}

impl Default for Hints {
    fn default() -> Self {
        Hints {
            background: Rgba::new(255, 220, 90, 230),
            foreground: Rgba::new(20, 20, 20, 255),
            foreground_dark: Rgba::new(240, 240, 240, 255),
            matched: Rgba::new(120, 220, 120, 230),
            font_family: "monospace".to_string(),
            font_size: 13.0,
            min_gap: 4,
        }
    }
}

impl Default for Ocr {
    fn default() -> Self {
        Ocr {
            binary: "tesseract".to_string(),
            language: "eng".to_string(),
            min_confidence: 50.0,
            min_element_size: 6,
            max_word_gap: 1.0,
            line_tolerance: 0.5,
        }
    }
}

impl Config {
    /// The conventional config path: `$XDG_CONFIG_HOME/mole/config.toml`,
    /// falling back to `~/.config/mole/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("mole").join("config.toml"))
    }

    /// Load and validate config from `path`. A missing file is not an error: the
    /// built-in defaults are returned instead.
    pub fn load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::info!("no config at {path:?}; using defaults");
                return Ok(Config::default());
            }
            Err(source) => {
                return Err(Error::ConfigIo {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        Config::from_toml(&text)
    }

    /// Parse and validate config from a TOML string.
    pub fn from_toml(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Reject configurations that would make the rest of the program misbehave.
    fn validate(&self) -> Result<()> {
        let alphabet: Vec<char> = self.keys.hint_alphabet.chars().collect();
        if alphabet.len() < 2 {
            return Err(Error::Config(
                "keys.hint_alphabet must contain at least 2 characters".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &alphabet {
            if !seen.insert(c) {
                return Err(Error::Config(format!(
                    "keys.hint_alphabet contains a duplicate character: {c:?}"
                )));
            }
        }
        if self.movement.step <= 0 || self.movement.large_step <= 0 {
            return Err(Error::Config("movement steps must be positive".into()));
        }
        if self.hints.font_size <= 0.0 {
            return Err(Error::Config("hints.font_size must be positive".into()));
        }
        if self.ocr.min_element_size < 0 {
            return Err(Error::Config(
                "ocr.min_element_size must not be negative".into(),
            ));
        }
        if !(0.0..=100.0).contains(&self.ocr.min_confidence) {
            return Err(Error::Config(
                "ocr.min_confidence must be between 0 and 100".into(),
            ));
        }
        if self.ocr.max_word_gap <= 0.0 || self.ocr.line_tolerance <= 0.0 {
            return Err(Error::Config(
                "ocr.max_word_gap and ocr.line_tolerance must be positive".into(),
            ));
        }
        Ok(())
    }

    /// The hint alphabet as a `Vec<char>`, ready for label generation.
    pub fn hint_alphabet(&self) -> Vec<char> {
        self.keys.hint_alphabet.chars().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../mole.example.toml");
        let cfg = Config::from_toml(text).expect("example config should parse");
        assert_eq!(cfg.keys.hint_alphabet, "asdfghjkl");
        assert_eq!(cfg.movement.large_step, 160);
        assert_eq!(cfg.ocr.language, "eng");
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg = Config::from_toml("[movement]\nstep = 5\n").unwrap();
        assert_eq!(cfg.movement.step, 5);
        // Untouched sections fall back to defaults.
        assert_eq!(cfg.keys.hint_alphabet, Keys::default().hint_alphabet);
    }

    #[test]
    fn duplicate_alphabet_char_is_rejected() {
        let err = Config::from_toml("[keys]\nhint_alphabet = \"aab\"\n").unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = Config::from_toml("[movement]\nnope = 1\n").unwrap_err();
        assert!(matches!(err, Error::ConfigParse(_)));
    }

    #[test]
    fn luminance_orders_black_below_white() {
        assert!(Rgba::new(0, 0, 0, 255).luminance() < Rgba::new(255, 255, 255, 255).luminance());
    }
}
