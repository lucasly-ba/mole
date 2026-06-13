//! Crate-wide error type.
//!
//! Every fallible operation in the library returns [`Result`], so callers can
//! propagate failures with `?` and the binary can render a single, consistent
//! error chain at the top level.

use std::path::PathBuf;

/// Convenient alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// All the ways mouseless can fail.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("could not read config file {path:?}: {source}")]
    ConfigIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("X11 error: {0}")]
    X11(String),

    #[error("AT-SPI / D-Bus error: {0}")]
    AtSpi(String),

    #[error("OCR error: {0}")]
    Ocr(String),

    #[error("rendering error: {0}")]
    Render(String),

    #[error("no element matched the typed keys")]
    NoMatch,

    #[error("the session was cancelled by the user")]
    Cancelled,

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Build an [`Error::X11`] from anything displayable.
    pub fn x11(e: impl std::fmt::Display) -> Self {
        Error::X11(e.to_string())
    }

    /// Build an [`Error::AtSpi`] from anything displayable.
    pub fn atspi(e: impl std::fmt::Display) -> Self {
        Error::AtSpi(e.to_string())
    }

    /// Build an [`Error::Render`] from anything displayable.
    pub fn render(e: impl std::fmt::Display) -> Self {
        Error::Render(e.to_string())
    }
}
