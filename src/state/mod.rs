//! Backups, the edit journal, undo, and the version lockfile.
//! Every edit to a user file goes through `safe_write`: back up, journal, then write atomically.

pub mod journal;
pub mod lockfile;
pub mod run;

pub use journal::{EditRecord, RunId};
pub use run::with_run_lock;

use std::path::{Path, PathBuf};

use std::fs::{self, OpenOptions};
use std::path::Component;

use anyhow::{Context, Result, anyhow};

use crate::ctx::Ctx;
use crate::fsx::write_atomic;

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
            // fs::copy gives the backup the target's mode. A read-only target would leave a backup
            // that a later copy in the same run cannot write over. Restore needs only the bytes, so
            // the backup gets 0600.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("cannot set the mode of {}", backup.display()))?;
            }
            // The copy must reach the disk before the journal record that names it is synced.
            // On Unix a read-only open can fsync, whatever the mode.
            // Windows needs write access for FlushFileBuffers; opening does not truncate.
            let opened = if cfg!(windows) {
                OpenOptions::new().write(true).open(&backup)
            } else {
                fs::File::open(&backup)
            };
            opened
                .and_then(|file| file.sync_all())
                .with_context(|| format!("cannot flush the backup {}", backup.display()))?;
            sync_backup_dirs(ctx, &backup)?;
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
    ensure_parent(&target)?;
    write_atomic(&target, text.as_bytes())
}

/// Flushes the directories from the backup's parent up to `dirs.home`, inclusive, so the backup's
/// entry, the directories made for it, and any fresh-home directories such as `.local/share`
/// survive a crash before the journal record is synced. `dirs.backups` is under `dirs.data`, which
/// is under `dirs.home`, so the walk reaches both.
fn sync_backup_dirs(ctx: &Ctx, backup: &Path) -> Result<()> {
    for dir in backup_dirs(ctx, backup) {
        sync_dir(&dir)?;
    }
    Ok(())
}

/// The directories `sync_backup_dirs` flushes: from the backup's parent up to `dirs.home`, inclusive.
fn backup_dirs(ctx: &Ctx, backup: &Path) -> Vec<PathBuf> {
    backup
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(&ctx.dirs.home))
        .map(Path::to_path_buf)
        .collect()
}

/// Flushes a directory, so entries created in it survive a crash.
#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("cannot flush the directory {}", path.display()))
}

/// Opening a directory to flush it is not possible on this platform, so there is nothing to do.
#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// Creates the directory that will hold `path`. `write_atomic` does not create directories,
/// so a download into a missing directory fails, while edits and undo still create them.
fn ensure_parent(path: &Path) -> Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display())),
        _ => Ok(()),
    }
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

/// Reverts a run (the last one when `run` is `None`), newest change first. Returns the touched paths.
/// The run lock is held for the whole undo, so no command that takes the run lock can write files
/// while they are reverted.
/// Inside it, the journal lock is held from the read to the rewrite, so an append from another
/// process cannot be lost in between. A dry run changes nothing and takes no lock.
pub fn undo(ctx: &Ctx, run: Option<&RunId>) -> Result<Vec<PathBuf>> {
    if ctx.dry_run {
        return revert(ctx, run);
    }
    with_run_lock(&ctx.dirs, || {
        journal::with_journal_lock(&ctx.dirs, || revert(ctx, run))
    })
}

/// The body of `undo`. The caller holds the journal lock unless this is a dry run.
fn revert(ctx: &Ctx, run: Option<&RunId>) -> Result<Vec<PathBuf>> {
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
    // Checked before any write, removal or keep backup, so a refused undo leaves the run whole.
    if let Some(record) = mine.iter().find(|record| record.file.is_symlink()) {
        return Err(symlink_refusal(&record.file));
    }
    // Every backup must open too, or undo would stop halfway through the run.
    check_backups(&mine)?;
    let mut touched = Vec::with_capacity(mine.len());
    for record in mine.iter().rev() {
        restore(ctx, record)?;
        touched.push(record.file.clone());
    }
    if !ctx.dry_run {
        journal::rewrite_under_lock(&ctx.dirs, &rest)?;
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

/// Checks the backup of every record that has one, newest first, the order undo restores them in,
/// so the first bad backup is the one named. A backup must be a regular file that opens. The
/// metadata check comes first, because opening a directory succeeds and opening a FIFO blocks.
/// Nothing is written here.
fn check_backups(mine: &[EditRecord]) -> Result<()> {
    for record in mine.iter().rev() {
        if let Some(backup) = &record.backup {
            let metadata = fs::metadata(backup)
                .with_context(|| format!("cannot read backup {}", backup.display()))?;
            if !metadata.is_file() {
                return Err(anyhow!("backup {} is not a regular file", backup.display()));
            }
            fs::File::open(backup)
                .with_context(|| format!("cannot read backup {}", backup.display()))?;
        }
    }
    Ok(())
}

/// The refusal for a journaled path that is now a symlink.
fn symlink_refusal(file: &Path) -> anyhow::Error {
    anyhow!(
        "{} is now a symlink; undo leaves symlinks alone, so it was not restored",
        file.display()
    )
}

/// Puts one journaled file back: the backup's bytes, or nothing if the file was new.
/// The current content is kept in a backup first.
fn restore(ctx: &Ctx, record: &EditRecord) -> Result<()> {
    // Undo neither writes through nor removes a symlink: the journal describes the file, not the
    // link's target, and a link put back by the user is not something undo may change.
    if record.file.is_symlink() {
        return Err(symlink_refusal(&record.file));
    }
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
            ensure_parent(&record.file)?;
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
    fn failed_rename_removes_the_temp_file_and_keeps_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target-dir");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside"), "kept").unwrap();

        // The temp file is created and written, then the rename onto a non-empty directory fails.
        let result = write_atomic(&target, b"new");

        assert!(result.is_err());
        assert!(
            target.join("inside").exists(),
            "the directory was disturbed"
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path())
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

    #[cfg(unix)]
    #[test]
    fn safe_write_edits_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444);
    }

    #[cfg(unix)]
    #[test]
    fn safe_write_syncs_backup_directories_on_a_fresh_run() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();
        let run = RunId::new();

        // No data or backup directory exists yet, so every directory on the walk is new.
        safe_write(&ctx, &run, &file, "new").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let records = journal::read(&ctx.dirs).unwrap();
        let backup = records[0].backup.clone().expect("backup path");
        assert_eq!(fs::read_to_string(backup).unwrap(), "old");
    }

    #[cfg(unix)]
    #[test]
    fn backup_of_a_read_only_file_is_owner_readable_and_writable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();

        let backup = journal::read(&ctx.dirs).unwrap()[0]
            .backup
            .clone()
            .expect("backup path");
        let mode = fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "backup {} has mode {mode:o}", backup.display());
    }

    #[cfg(unix)]
    #[test]
    fn retry_in_the_same_run_can_copy_over_a_read_only_target_backup() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        let run = RunId::new();
        safe_write(&ctx, &run, &file, "new").unwrap();
        // Forces the path where the journal does not yet name the backup, as after an interrupted
        // attempt. The next safe_write in this run copies the target over the existing backup.
        fs::remove_file(ctx.dirs.data.join("journal.jsonl")).unwrap();

        safe_write(&ctx, &run, &file, "newer").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
    }

    #[test]
    fn backup_dirs_lists_the_directories_up_to_home_and_nothing_above() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let backup = ctx
            .dirs
            .backups
            .join("run-1")
            .join("nested")
            .join("settings.json");

        let dirs = backup_dirs(&ctx, &backup);

        assert_eq!(dirs.first().map(PathBuf::as_path), backup.parent());
        assert_eq!(dirs.last(), Some(&ctx.dirs.home));
        assert!(dirs.contains(&ctx.dirs.backups));
        assert!(dirs.contains(&ctx.dirs.data));
        assert!(
            dirs.iter().all(|d| d.starts_with(&ctx.dirs.home)),
            "a directory above home is listed: {dirs:?}"
        );
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
    fn undo_recreates_a_deleted_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let sub = dir.path().join("sub");
        let file = sub.join("settings.json");
        fs::create_dir(&sub).unwrap();
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();
        fs::remove_dir_all(&sub).unwrap();

        undo(&ctx, None).unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "orig");
    }

    #[test]
    fn undo_waits_for_the_journal_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ctx = test_ctx(&root);
        let file = root.join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();
        let holder = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(ctx.dirs.data.join("journal.lock"))
            .unwrap();
        holder.lock().unwrap();

        let writer_root = root.clone();
        let writer = std::thread::spawn(move || undo(&test_ctx(&writer_root), None).map(|_| ()));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !writer.is_finished(),
            "undo ran while another handle held the journal lock"
        );

        holder.unlock().unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "orig");
    }

    #[cfg(unix)]
    #[test]
    fn undo_refuses_a_journaled_path_that_is_now_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let file = dir.path().join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "setup").unwrap();
        // The user moves the file to a dotfiles copy and links it back.
        let linked = dir.path().join("dotfiles-settings.json");
        fs::rename(&file, &linked).unwrap();
        fs::write(&linked, "other bytes").unwrap();
        std::os::unix::fs::symlink(&linked, &file).unwrap();

        let error = undo(&ctx, None).expect_err("undo wrote through a symlink");

        let message = format!("{error:#}");
        assert!(
            message.contains("undo leaves symlinks alone"),
            "message: {message}"
        );
        assert!(
            message.contains(&file.display().to_string()),
            "message: {message}"
        );
        assert_eq!(fs::read_to_string(&linked).unwrap(), "other bytes");
        assert_eq!(journal::read(&ctx.dirs).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn undo_checks_every_record_for_symlinks_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let run = RunId::new();
        let linked_later = dir.path().join("settings.json");
        let regular = dir.path().join("other.json");
        fs::write(&linked_later, "orig").unwrap();
        fs::write(&regular, "orig").unwrap();
        // Journal order is settings.json then other.json; undo restores newest first, so
        // other.json would be restored before the symlink is reached.
        safe_write(&ctx, &run, &linked_later, "setup").unwrap();
        safe_write(&ctx, &run, &regular, "setup 2").unwrap();
        let moved = dir.path().join("dotfiles-settings.json");
        fs::rename(&linked_later, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &linked_later).unwrap();
        let backups_before = backup_dir_names(&ctx);

        let error = undo(&ctx, None).expect_err("undo ignored a symlinked record");

        let message = format!("{error:#}");
        assert!(
            message.contains("undo leaves symlinks alone"),
            "message: {message}"
        );
        assert_eq!(fs::read_to_string(&regular).unwrap(), "setup 2");
        assert_eq!(fs::read_to_string(&moved).unwrap(), "setup");
        assert_eq!(journal::read(&ctx.dirs).unwrap().len(), 2);
        assert_eq!(
            backup_dir_names(&ctx),
            backups_before,
            "a keep backup was written"
        );
    }

    /// The names of everything under the backups directory, sorted.
    fn backup_dir_names(ctx: &Ctx) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(&ctx.dirs.backups) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    #[test]
    fn undo_checks_every_backup_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let run = RunId::new();
        let older = dir.path().join("older.json");
        let newer = dir.path().join("newer.json");
        fs::write(&older, "orig").unwrap();
        fs::write(&newer, "orig").unwrap();
        // Journal order is older, created, newer, so undo restores newer, created, then older.
        safe_write(&ctx, &run, &older, "setup").unwrap();
        let created = dir.path().join("created.json");
        safe_write(&ctx, &run, &created, "fresh").unwrap();
        safe_write(&ctx, &run, &newer, "setup").unwrap();
        let older_backup = journal::read(&ctx.dirs)
            .unwrap()
            .into_iter()
            .find(|record| record.file == older)
            .and_then(|record| record.backup)
            .expect("backup of the older file");
        fs::remove_file(&older_backup).unwrap();
        let journal_file = ctx.dirs.data.join("journal.jsonl");
        let journal_before = fs::read(&journal_file).unwrap();
        let newer_before = fs::read(&newer).unwrap();
        let backups_before = backup_dir_names(&ctx);

        let error = undo(&ctx, None).expect_err("undo ran with a missing backup");

        let message = format!("{error:#}");
        assert!(
            message.contains(&older_backup.display().to_string()),
            "message: {message}"
        );
        assert_eq!(
            fs::read(&newer).unwrap(),
            newer_before,
            "the newer file was restored before the missing backup was found"
        );
        assert_eq!(
            fs::read(&journal_file).unwrap(),
            journal_before,
            "the journal changed"
        );
        assert!(created.exists(), "the created file was removed");
        assert_eq!(
            backup_dir_names(&ctx),
            backups_before,
            "a keep backup was written"
        );
    }

    #[test]
    fn undo_refuses_a_backup_that_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let run = RunId::new();
        let older = dir.path().join("older.json");
        let newer = dir.path().join("newer.json");
        fs::write(&older, "orig").unwrap();
        fs::write(&newer, "orig").unwrap();
        safe_write(&ctx, &run, &older, "setup").unwrap();
        safe_write(&ctx, &run, &newer, "setup").unwrap();
        let older_backup = journal::read(&ctx.dirs)
            .unwrap()
            .into_iter()
            .find(|record| record.file == older)
            .and_then(|record| record.backup)
            .expect("backup of the older file");
        fs::remove_file(&older_backup).unwrap();
        fs::create_dir(&older_backup).unwrap();
        let newer_before = fs::read(&newer).unwrap();

        let error = undo(&ctx, None).expect_err("undo accepted a directory as a backup");

        let message = format!("{error:#}");
        assert!(
            message.contains(&older_backup.display().to_string()),
            "message: {message}"
        );
        assert_eq!(
            fs::read(&newer).unwrap(),
            newer_before,
            "the newer file was restored before the bad backup was found"
        );
    }

    #[test]
    fn undo_waits_for_the_run_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ctx = test_ctx(&root);
        let file = root.join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();
        let holder = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(ctx.dirs.data.join("run.lock"))
            .unwrap();
        holder.lock().unwrap();

        let writer_root = root.clone();
        let writer = std::thread::spawn(move || undo(&test_ctx(&writer_root), None).map(|_| ()));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !writer.is_finished(),
            "undo ran while another handle held the run lock"
        );

        holder.unlock().unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "orig");
    }

    #[test]
    fn dry_run_undo_does_not_take_or_create_the_run_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut ctx = test_ctx(&root);
        let file = root.join("settings.json");
        fs::write(&file, "orig").unwrap();
        safe_write(&ctx, &RunId::new(), &file, "new").unwrap();
        let lock = ctx.dirs.data.join("run.lock");
        assert!(!lock.exists(), "setup created the run lock");
        ctx.dry_run = true;

        undo(&ctx, None).unwrap();
        assert!(!lock.exists(), "dry-run undo created the run lock");

        // Held by another handle, a dry run must still finish: it takes no lock.
        let holder = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        holder.lock().unwrap();
        let writer_root = root.clone();
        let writer = std::thread::spawn(move || {
            let mut ctx = test_ctx(&writer_root);
            ctx.dry_run = true;
            undo(&ctx, None).map(|_| ())
        });
        for _ in 0..200 {
            if writer.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let finished = writer.is_finished();
        holder.unlock().unwrap();
        writer.join().unwrap().unwrap();
        assert!(finished, "dry-run undo waited for the run lock");
    }

    #[test]
    fn undo_of_an_unknown_run_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let missing: RunId = "1-1".parse().unwrap();
        assert!(undo(&ctx, Some(&missing)).is_err());
    }
}
