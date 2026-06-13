//! The daemon's wire protocol (Plan §5.2).
//!
//! A deliberately tiny line protocol over a Unix socket: the client writes one
//! command word and reads one status line. This is all i3 needs
//! (`bindsym $mod+a exec mouseless click`) and keeps the daemon dependency-free.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::session::Mode;
use crate::x11::Button;

/// A command the client can send the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Show hints and teleport the pointer to the chosen one.
    Hint(Mode),
    /// Free hjkl pointer movement.
    FreeMove,
    /// Re-read the config file now.
    Reload,
    /// Liveness check.
    Ping,
}

impl Command {
    /// Parse the wire word into a command.
    pub fn parse(word: &str) -> Result<Command> {
        let cmd = match word.trim() {
            "teleport" | "hint" => Command::Hint(Mode::Teleport),
            "click" => Command::Hint(Mode::Click {
                button: Button::Left,
                count: 1,
            }),
            "double-click" => Command::Hint(Mode::Click {
                button: Button::Left,
                count: 2,
            }),
            "right-click" => Command::Hint(Mode::Click {
                button: Button::Right,
                count: 1,
            }),
            "drag" => Command::Hint(Mode::Drag {
                button: Button::Left,
            }),
            "move" => Command::FreeMove,
            "reload" => Command::Reload,
            "ping" => Command::Ping,
            other => return Err(Error::Ipc(format!("unknown command {other:?}"))),
        };
        Ok(cmd)
    }

    /// The wire word for this command (inverse of [`Command::parse`]).
    pub fn as_word(&self) -> &'static str {
        match self {
            Command::Hint(Mode::Teleport) => "teleport",
            Command::Hint(Mode::Click {
                button: Button::Left,
                count: 1,
            }) => "click",
            Command::Hint(Mode::Click {
                button: Button::Left,
                count: 2,
            }) => "double-click",
            Command::Hint(Mode::Click {
                button: Button::Right,
                ..
            }) => "right-click",
            Command::Hint(Mode::Click { .. }) => "click",
            Command::Hint(Mode::Drag { .. }) => "drag",
            Command::FreeMove => "move",
            Command::Reload => "reload",
            Command::Ping => "ping",
        }
    }
}

/// Resolve the socket path: explicit config, else `$XDG_RUNTIME_DIR`, else
/// a per-user path in `/tmp`.
pub fn socket_path(config: &Config) -> PathBuf {
    if let Some(p) = &config.daemon.socket {
        return p.clone();
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("mouseless.sock");
    }
    let uid = unsafe { libc_getuid() };
    PathBuf::from(format!("/tmp/mouseless-{uid}.sock"))
}

// Avoid pulling in the `libc` crate for a single call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Client side: connect, send `command`, return the daemon's status line.
pub fn send(path: &std::path::Path, command: Command) -> Result<String> {
    let mut stream = UnixStream::connect(path).map_err(|e| {
        Error::Ipc(format!(
            "could not connect to daemon at {path:?} ({e}); is `mouseless daemon` running?"
        ))
    })?;
    writeln!(stream, "{}", command.as_word()).map_err(|e| Error::Ipc(e.to_string()))?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| Error::Ipc(e.to_string()))?;
    Ok(response.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_words_roundtrip() {
        for word in [
            "teleport",
            "click",
            "double-click",
            "right-click",
            "drag",
            "move",
            "reload",
            "ping",
        ] {
            let cmd = Command::parse(word).unwrap();
            assert_eq!(cmd.as_word(), word, "roundtrip failed for {word}");
        }
    }

    #[test]
    fn unknown_command_errors() {
        assert!(Command::parse("frobnicate").is_err());
    }

    #[test]
    fn hint_alias_maps_to_teleport() {
        assert_eq!(Command::parse("hint").unwrap(), Command::Hint(Mode::Teleport));
    }
}
