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
#[serde(try_from = "String")]
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

/// Deserialization goes through `FromStr`, so a journal line cannot carry an unsafe id.
impl TryFrom<String> for RunId {
    type Error = anyhow::Error;

    fn try_from(s: String) -> Result<Self> {
        s.parse()
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
/// A last line that does not parse and has no trailing newline is a torn append from an
/// interrupted write. It is skipped. Any other unparseable line is an error.
pub fn read(dirs: &KitDirs) -> Result<Vec<EditRecord>> {
    let path = journal_path(dirs);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let lines: Vec<&str> = text.lines().collect();
    let torn_tail = !text.ends_with('\n');
    let mut records = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) if torn_tail && index + 1 == lines.len() => break,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "{}: line {} is not a journal record",
                        path.display(),
                        index + 1
                    )
                });
            }
        }
    }
    Ok(records)
}

fn journal_path(dirs: &KitDirs) -> PathBuf {
    dirs.data.join("journal.jsonl")
}

/// The journal file's directory is `dirs.data`, created on first use.
fn ensure_dir(dirs: &KitDirs) -> Result<()> {
    fs::create_dir_all(&dirs.data).with_context(|| format!("cannot create {}", dirs.data.display()))
}

/// True when the journal ends in a partial line, which an interrupted append leaves behind.
fn has_torn_tail(dirs: &KitDirs) -> Result<bool> {
    let path = journal_path(dirs);
    match fs::read(&path) {
        Ok(bytes) => Ok(!bytes.is_empty() && bytes.last() != Some(&b'\n')),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Appends one record. A torn last line is dropped first, so it never ends up mid-file.
pub fn append(dirs: &KitDirs, record: &EditRecord) -> Result<()> {
    ensure_dir(dirs)?;
    if has_torn_tail(dirs)? {
        rewrite(dirs, &read(dirs)?)?;
    }
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
    crate::fsx::write_atomic(&journal_path(dirs), text.as_bytes())
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

    fn sample_record(run: &str, file: &str) -> EditRecord {
        EditRecord {
            run: run.parse().unwrap(),
            file: PathBuf::from(file),
            backup: None,
            created: true,
        }
    }

    fn journal_in(root: &std::path::Path, text: &str) -> KitDirs {
        let dirs = KitDirs::rooted(root);
        fs::create_dir_all(&dirs.data).unwrap();
        fs::write(journal_path(&dirs), text).unwrap();
        dirs
    }

    #[test]
    fn read_skips_a_torn_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&sample_record("1-1", "/h/a.json")).unwrap();
        let dirs = journal_in(dir.path(), &format!("{good}\n{{\"run\":\"17"));

        let records = read(&dirs).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run.as_str(), "1-1");
    }

    #[test]
    fn read_still_errors_on_a_bad_line_in_the_middle() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&sample_record("1-1", "/h/a.json")).unwrap();
        let dirs = journal_in(dir.path(), &format!("{good}\nnot json\n{good}\n"));

        assert!(read(&dirs).is_err());
    }

    #[test]
    fn append_after_a_torn_line_keeps_the_journal_readable() {
        let dir = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&sample_record("1-1", "/h/a.json")).unwrap();
        let dirs = journal_in(dir.path(), &format!("{good}\n{{\"run\":\"17"));

        append(&dirs, &sample_record("2-2", "/h/b.json")).unwrap();

        let records = read(&dirs).unwrap();
        let runs: Vec<&str> = records.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(runs, vec!["1-1", "2-2"]);
    }

    #[test]
    fn journal_line_with_a_path_like_run_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(dir.path());
        fs::create_dir_all(&dirs.data).unwrap();
        let line = r#"{"run":"../x","file":"/h/settings.json","backup":null,"created":true}"#;
        fs::write(journal_path(&dirs), format!("{line}\n")).unwrap();

        assert!(read(&dirs).is_err());
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
