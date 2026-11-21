pub mod brand;
pub mod cli;
pub mod ctx;
pub mod edit;
pub mod exec;
pub mod host;
pub mod net;
pub mod platform;
pub mod state;

use anyhow::{Result, anyhow};

use cli::{Cli, Command};

/// Runs a parsed command. Each handler is a stub until its task lands.
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Setup
        | Command::Status
        | Command::Doctor
        | Command::Update { .. }
        | Command::Config(_)
        | Command::Preset { .. }
        | Command::Privacy { .. }
        | Command::Login { .. }
        | Command::Undo { .. }
        | Command::Uninstall
        | Command::Usage { .. } => Err(anyhow!("not implemented")),
    }
}
