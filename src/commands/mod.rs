//! The command handlers behind the CLI: setup, status and doctor.

pub mod doctor;
pub mod setup;
pub mod status;
pub mod update;

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
