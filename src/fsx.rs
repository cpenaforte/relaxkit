//! File writes that never leave a half-written file behind.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// Writes `bytes` to a temp file beside `target`, then renames it over `target`.
/// On failure the temp file is removed and `target` is left as it was.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
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
