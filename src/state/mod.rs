//! Backups, the edit journal, undo, and the version lockfile.
//! Every edit to a user file goes through `safe_write`: back up, journal, then write atomically.

pub mod journal;
pub mod lockfile;

pub use journal::{EditRecord, RunId};

use std::path::{Path, PathBuf};

use std::fs;
use std::io::Write;
use std::path::Component;

use anyhow::{Context, Result, anyhow};

use crate::ctx::Ctx;

/// Writes `text` to `path`, backing up the old file under the run and journaling the change.
pub fn safe_write(ctx: &Ctx, run: &RunId, path: &Path, text: &str) -> Result<()> {
    if ctx.dry_run {
        ctx.out.info(&format!("would write {}", path.display()));
        return Ok(());
    }
    let target = resolve(path)?;
    let journaled = journal::read(&ctx.dirs)?
        .iter()
        .any(|record| record.run == *run && record.file == target);
    if !journaled {
        // Only the first write to a file in a run is journaled, so its backup is the original.
        let record = if target.exists() {
            let backup = backup_path(ctx, run, &target);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            fs::copy(&target, &backup)
                .with_context(|| format!("cannot back up {}", target.display()))?;
            EditRecord {
                run: run.clone(),
                file: target.clone(),
                backup: Some(backup),
                created: false,
            }
        } else {
            EditRecord {
                run: run.clone(),
                file: target.clone(),
                backup: None,
                created: true,
            }
        };
        journal::append(&ctx.dirs, &record)?;
    }
    write_atomic(&target, text.as_bytes())
}

/// A symlink is followed, so the link itself survives the write. The result is absolute,
/// so a journaled path means the same file when undo runs from another directory.
fn resolve(path: &Path) -> Result<PathBuf> {
    if path.is_symlink() {
        fs::canonicalize(path).with_context(|| format!("cannot follow the link {}", path.display()))
    } else {
        std::path::absolute(path)
            .with_context(|| format!("cannot make {} absolute", path.display()))
    }
}

/// `dirs.backups/<run>/` followed by the target path without its root or any `..`.
fn backup_path(ctx: &Ctx, run: &RunId, target: &Path) -> PathBuf {
    let relative: PathBuf = target
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    ctx.dirs.backups.join(run.as_str()).join(relative)
}

/// Writes `bytes` to a temp file beside `target`, then renames it over `target`.
/// On failure the temp file is removed and `target` is left as it was.
fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let parent = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", target.display()))?;
    let temp = parent.join(format!("{}.relaxkit-tmp", name.to_string_lossy()));
    let result = write_temp_then_rename(&temp, target, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_temp_then_rename(temp: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let mut file =
        fs::File::create(temp).with_context(|| format!("cannot create {}", temp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("cannot write {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("cannot flush {}", temp.display()))?;
    drop(file);
    if let Ok(meta) = fs::metadata(target) {
        fs::set_permissions(temp, meta.permissions())
            .with_context(|| format!("cannot copy the mode of {}", target.display()))?;
    }
    fs::rename(temp, target).with_context(|| format!("cannot replace {}", target.display()))
}

/// Reverts a run (the last one when `run` is `None`), newest change first. Returns the touched paths.
pub fn undo(ctx: &Ctx, run: Option<&RunId>) -> Result<Vec<PathBuf>> {
    let records = journal::read(&ctx.dirs)?;
    let target = match run {
        Some(run) => run.clone(),
        None => match records.last() {
            Some(last) => last.run.clone(),
            None => return Ok(Vec::new()),
        },
    };
    let (mine, rest): (Vec<EditRecord>, Vec<EditRecord>) =
        records.into_iter().partition(|record| record.run == target);
    if mine.is_empty() {
        return Err(anyhow!("no journaled changes for run {target}"));
    }
    let mut touched = Vec::with_capacity(mine.len());
    for record in mine.iter().rev() {
        restore(ctx, record)?;
        touched.push(record.file.clone());
    }
    if !ctx.dry_run {
        journal::rewrite(&ctx.dirs, &rest)?;
        let backups = ctx.dirs.backups.join(target.as_str());
        if backups.exists() {
            fs::remove_dir_all(&backups)
                .with_context(|| format!("cannot remove {}", backups.display()))?;
        }
    }
    Ok(touched)
}

/// Copies the current content of `file` to a fresh backup and reports it, so that undo
/// never discards a hand edit. Does nothing when the file is absent.
fn keep_current(ctx: &Ctx, file: &Path) -> Result<()> {
    if !file.exists() {
        return Ok(());
    }
    let backup = backup_path(ctx, &RunId::new(), file);
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::copy(file, &backup).with_context(|| format!("cannot back up {}", file.display()))?;
    ctx.out.info(&format!(
        "kept the current {} as {}",
        file.display(),
        backup.display()
    ));
    Ok(())
}

/// Puts one journaled file back: the backup's bytes, or nothing if the file was new.
/// The current content is kept in a backup first.
fn restore(ctx: &Ctx, record: &EditRecord) -> Result<()> {
    match (&record.backup, record.created) {
        (Some(backup), _) => {
            if ctx.dry_run {
                ctx.out
                    .info(&format!("would restore {}", record.file.display()));
                return Ok(());
            }
            let bytes = fs::read(backup)
                .with_context(|| format!("cannot read backup {}", backup.display()))?;
            keep_current(ctx, &record.file)?;
            write_atomic(&record.file, &bytes)
        }
        (None, true) => {
            if ctx.dry_run {
                ctx.out
                    .info(&format!("would remove {}", record.file.display()));
                return Ok(());
            }
            keep_current(ctx, &record.file)?;
            match fs::remove_file(&record.file) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => {
                    Err(err).with_context(|| format!("cannot remove {}", record.file.display()))
                }
            }
        }
        (None, false) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::state::journal;

    fn test_ctx(root: &Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    #[test]
    fn safe_write_backs_up_existing_file_and_journals() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();
        let run = RunId::new();

        safe_write(&ctx, &run, &file, "new").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let records = journal::read(&ctx.dirs).unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].created);
        let backup = records[0].backup.clone().expect("backup path");
        assert_eq!(fs::read_to_string(backup).unwrap(), "old");
    }

    #[test]
    fn safe_write_records_created_file_and_undo_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("fresh").join("config.toml");
        let run = RunId::new();

        safe_write(&ctx, &run, &file, "x = 1\n").unwrap();
        let records = journal::read(&ctx.dirs).unwrap();
        assert!(records[0].created);
        assert!(records[0].backup.is_none());
        assert!(file.exists());

        undo(&ctx, None).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn undo_restores_original_after_two_writes_in_one_run() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let existing = dir.path().join("existing.json");
        let created = dir.path().join("created.json");
        fs::write(&existing, "orig").unwrap();
        let run = RunId::new();

        safe_write(&ctx, &run, &existing, "one").unwrap();
        safe_write(&ctx, &run, &existing, "two").unwrap();
        safe_write(&ctx, &run, &created, "fresh").unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "two");

        let touched = undo(&ctx, None).unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "orig");
        assert!(!created.exists());
        assert_eq!(touched.len(), 2);
    }

    #[test]
    fn dry_run_writes_nothing_but_reports() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_ctx(dir.path());
        ctx.dry_run = true;
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();

        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
        assert!(!ctx.dirs.backups.exists());
        assert!(journal::read(&ctx.dirs).unwrap().is_empty());
        assert_eq!(
            ctx.out.lines(),
            vec![format!("would write {}", file.display())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_leaves_no_temp_file_and_keeps_the_original() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        let file = locked.join("settings.json");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        // Root and other users who ignore permissions can still write to a 0555 directory.
        let probe = locked.join("probe");
        if fs::File::create(&probe).is_ok() {
            let _ = fs::remove_file(&probe);
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            println!("skipped: running as a user who ignores permissions");
            return;
        }

        let result = safe_write(&ctx, &RunId::new(), &file, "new");

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
        let leftovers: Vec<_> = fs::read_dir(&locked)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".relaxkit-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn safe_write_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        safe_write(&ctx, &RunId::new(), &link, "new").unwrap();

        assert!(link.is_symlink(), "the link was replaced by a regular file");
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    /// True when some file under `root` holds exactly `text`.
    fn backup_holds(root: &Path, text: &str) -> bool {
        fn walk(dir: &Path, text: &str) -> bool {
            let Ok(entries) = fs::read_dir(dir) else {
                return false;
            };
            entries.flatten().any(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, text)
                } else {
                    fs::read_to_string(&path).is_ok_and(|content| content == text)
                }
            })
        }
        walk(root, text)
    }

    #[test]
    fn undo_keeps_a_hand_edit_to_a_restored_file_in_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "setup").unwrap();
        fs::write(&file, "hand edit\n").unwrap();

        undo(&ctx, None).unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "orig");
        assert!(
            backup_holds(&ctx.dirs.backups, "hand edit\n"),
            "the hand edit was discarded without a backup"
        );
    }

    #[test]
    fn undo_keeps_a_hand_edit_to_a_created_file_before_removing_it() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("fresh").join("config.toml");
        safe_write(&ctx, &RunId::new(), &file, "x = 1\n").unwrap();
        fs::write(&file, "added by hand\n").unwrap();

        undo(&ctx, None).unwrap();

        assert!(!file.exists());
        assert!(
            backup_holds(&ctx.dirs.backups, "added by hand\n"),
            "the hand edit was removed without a backup"
        );
    }

    #[test]
    fn dry_run_undo_writes_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "setup").unwrap();
        fs::write(&file, "hand edit\n").unwrap();
        ctx.dry_run = true;

        undo(&ctx, None).unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "hand edit\n");
        assert!(!backup_holds(&ctx.dirs.backups, "hand edit\n"));
        assert_eq!(journal::read(&ctx.dirs).unwrap().len(), 1);
    }

    #[test]
    fn a_relative_path_is_journaled_as_absolute() {
        let resolved = resolve(Path::new("some/relative/settings.json")).unwrap();
        assert!(
            resolved.is_absolute(),
            "journaled as {}",
            resolved.display()
        );
        assert!(resolved.ends_with("some/relative/settings.json"));
    }

    #[test]
    fn undo_of_an_unknown_run_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let missing: RunId = "1-1".parse().unwrap();
        assert!(undo(&ctx, Some(&missing)).is_err());
    }
}
