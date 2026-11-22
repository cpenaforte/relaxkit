//! The command handlers behind the CLI.

pub mod config;
pub mod doctor;
pub mod login;
pub mod preset;
pub mod privacy;
pub mod setup;
pub mod status;
pub mod undo;
pub mod uninstall;
pub mod update;
pub mod usage;

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

/// The text of a file, or `None` when it does not exist. Any other read error is reported.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}
