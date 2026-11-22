//! The run-level lock: held for the whole of a command that changes kit state, so one command
//! cannot revert or rewrite files while another is still writing them.

use std::fs::{self, OpenOptions};

use anyhow::{Context, Result};

use crate::platform::KitDirs;

/// Runs `body` while holding the exclusive run lock at `dirs.data/run.lock`. The lock is separate
/// from the journal lock: `safe_write` and `journal::append` take only the journal lock, so a
/// command that holds the run lock can call them without a reentrant lock.
pub fn with_run_lock<T>(dirs: &KitDirs, body: impl FnOnce() -> Result<T>) -> Result<T> {
    fs::create_dir_all(&dirs.data)
        .with_context(|| format!("cannot create {}", dirs.data.display()))?;
    let path = dirs.data.join("run.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("cannot lock {}", path.display()))?;
    let result = body();
    drop(file);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn run_lock_is_released_when_the_body_returns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let dirs = KitDirs::rooted(&root);

        assert_eq!(with_run_lock(&dirs, || Ok(7)).unwrap(), 7);

        // A second handle in another thread can take the lock only if the first was released.
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            let dirs = KitDirs::rooted(&root);
            sent.send(with_run_lock(&dirs, || Ok(()))).unwrap();
        });
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("the run lock was not released")
            .unwrap();
    }
}
