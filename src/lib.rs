pub mod brand;
pub mod cli;
pub mod config;
pub mod config_apply;
pub mod ctx;
pub mod edit;
pub mod exec;
pub mod fsx;
pub mod github;
pub mod host;
pub mod net;
pub mod platform;
pub mod presets;
pub mod recipe;
pub mod roles;
pub mod runtime;
pub mod sandbox;
pub mod state;
pub mod tools;

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
