//! mole command-line entry point.
//!
//! One binary, two roles:
//!
//! * `mole daemon`: the long-running background process.
//! * everything else (`click`, `teleport`, `drag`, …): a thin client that
//!   sends one command to the daemon over its Unix socket. This is what i3 (or
//!   any WM) execs from a keybinding.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use mole::config::Config;
use mole::daemon::ipc::{self, Command as IpcCommand};
use mole::daemon::Daemon;
use mole::error::Result;

#[derive(Parser)]
#[command(
    name = "mole",
    version,
    about = "Keyboard-only mouse navigation for Linux/X11"
)]
struct Cli {
    /// Path to the config file (defaults to ~/.config/mole/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background daemon.
    Daemon,
    /// Show hints and teleport the pointer to the chosen element.
    Teleport,
    /// Show hints and left-click the chosen element.
    Click,
    /// Show hints and double-click the chosen element.
    DoubleClick,
    /// Show hints and right-click the chosen element.
    RightClick,
    /// Pick two hints and drag between them (copies the selection).
    Drag,
    /// Free hjkl pointer movement.
    Move,
    /// Ask the daemon to reload its config now.
    Reload,
    /// Check that the daemon is alive.
    Ping,
    /// Print the default configuration to stdout.
    DumpConfig,
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mole: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);

    match cli.command {
        Cmd::Daemon => {
            let config = Config::load(&config_path)?;
            Daemon::new(config, config_path).run()
        }
        Cmd::DumpConfig => {
            let toml = toml::to_string_pretty(&Config::default())
                .map_err(|e| mole::Error::Config(e.to_string()))?;
            print!("{toml}");
            Ok(())
        }
        client => {
            let command = client_command(&client);
            let config = Config::load(&config_path)?;
            let socket = ipc::socket_path(&config);
            let response = ipc::send(&socket, command)?;
            println!("{response}");
            if response.starts_with("err:") {
                return Err(mole::Error::Ipc(response));
            }
            Ok(())
        }
    }
}

/// Map a client subcommand to its IPC command.
fn client_command(cmd: &Cmd) -> IpcCommand {
    use mole::session::Mode;
    use mole::x11::Button;
    match cmd {
        Cmd::Teleport => IpcCommand::Hint(Mode::Teleport),
        Cmd::Click => IpcCommand::Hint(Mode::Click {
            button: Button::Left,
            count: 1,
        }),
        Cmd::DoubleClick => IpcCommand::Hint(Mode::Click {
            button: Button::Left,
            count: 2,
        }),
        Cmd::RightClick => IpcCommand::Hint(Mode::Click {
            button: Button::Right,
            count: 1,
        }),
        Cmd::Drag => IpcCommand::Hint(Mode::Drag {
            button: Button::Left,
        }),
        Cmd::Move => IpcCommand::FreeMove,
        Cmd::Reload => IpcCommand::Reload,
        Cmd::Ping => IpcCommand::Ping,
        // Non-client commands are handled before this is ever called.
        Cmd::Daemon | Cmd::DumpConfig => unreachable!("not a client command"),
    }
}

/// Default config path, with a sane fallback if `$HOME` is unset.
fn default_config_path() -> PathBuf {
    Config::default_path().unwrap_or_else(|| PathBuf::from("mole.toml"))
}
