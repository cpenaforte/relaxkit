//! File writes that never leave a half-written file behind.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// Writes `bytes` to a temp file beside `target`, then renames it over `target`.
/// On failure the temp file is removed and `target` is left as it was.
/// A symlink target is resolved first, so the rename does not replace the link.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let target = resolve_link(target)?;
    let target = target.as_path();
    let parent = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
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

/// The file a symlink points to, so a rename does not turn the link into a regular file.
/// A dangling link is an error, since there is no file to write.
fn resolve_link(target: &Path) -> Result<PathBuf> {
    if target.is_symlink() {
        fs::canonicalize(target)
            .with_context(|| format!("cannot follow the link {}", target.display()))
    } else {
        Ok(target.to_path_buf())
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_keeps_a_symlink_and_writes_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("a.json");
        let link = dir.path().join("link.json");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, b"new").unwrap();

        assert!(link.is_symlink(), "the link was replaced by a regular file");
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }
}
