//! The run id and the append-only journal of file changes that `undo` reads.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::platform::KitDirs;

/// Identifies one relaxkit run. Sortable: a zero-padded timestamp comes first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Epoch milliseconds, then the process id and a per-process counter, so ids sort by time.
    pub fn new() -> RunId {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        RunId(format!("{millis:013}-{:06}-{seq:06}", std::process::id()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        RunId::new()
    }
}

impl FromStr for RunId {
    type Err = anyhow::Error;

    /// Only digits and hyphens are accepted, so an id is always a safe directory name.
    fn from_str(s: &str) -> Result<Self> {
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-') {
            Ok(RunId(s.to_string()))
        } else {
            Err(anyhow!("invalid run id `{s}`"))
        }
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One journaled change: the file, where its old text was backed up, and whether it was new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRecord {
    pub run: RunId,
    pub file: PathBuf,
    pub backup: Option<PathBuf>,
    pub created: bool,
}

/// All records in order, oldest first. A missing journal is empty.
pub fn read(dirs: &KitDirs) -> Result<Vec<EditRecord>> {
    let path = journal_path(dirs);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(number, line)| {
            serde_json::from_str(line).with_context(|| {
                format!(
                    "{}: line {} is not a journal record",
                    path.display(),
                    number + 1
                )
            })
        })
        .collect()
}

fn journal_path(dirs: &KitDirs) -> PathBuf {
    dirs.data.join("journal.jsonl")
}

/// The journal file's directory is `dirs.data`, created on first use.
fn ensure_dir(dirs: &KitDirs) -> Result<()> {
    fs::create_dir_all(&dirs.data).with_context(|| format!("cannot create {}", dirs.data.display()))
}

/// Appends one record.
pub fn append(dirs: &KitDirs, record: &EditRecord) -> Result<()> {
    ensure_dir(dirs)?;
    let path = journal_path(dirs);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let line = serde_json::to_string(record).context("cannot encode journal record")?;
    writeln!(file, "{line}").with_context(|| format!("cannot append to {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("cannot flush {}", path.display()))
}

/// Replaces the whole journal with `records`.
pub fn rewrite(dirs: &KitDirs, records: &[EditRecord]) -> Result<()> {
    ensure_dir(dirs)?;
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).context("cannot encode journal record")?);
        text.push('\n');
    }
    super::write_atomic(&journal_path(dirs), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_sort_in_creation_order() {
        let first = RunId::new();
        let second = RunId::new();
        assert!(first < second);
    }

    #[test]
    fn run_id_rejects_path_like_input() {
        assert!("../etc".parse::<RunId>().is_err());
        assert!("".parse::<RunId>().is_err());
        assert_eq!(
            "1763769600000-1-000001".parse::<RunId>().unwrap().as_str(),
            "1763769600000-1-000001"
        );
    }
}
