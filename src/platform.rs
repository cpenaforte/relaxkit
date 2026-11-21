//! OS-specific code. Linux and macOS only in v1; everything else is rejected here.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::brand;

/// The directories relaxkit reads and writes, rooted at the user's home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitDirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub bin: PathBuf,
    pub backups: PathBuf,
}

impl KitDirs {
    /// Resolves the layout under the current user's home directory.
    pub fn discover() -> Result<KitDirs> {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| anyhow!("cannot find the home directory"))?;
        Ok(KitDirs::rooted(base.home_dir()))
    }

    /// Builds the same layout under an arbitrary root. Used by tests and by `discover`.
    pub fn rooted(root: &Path) -> KitDirs {
        let config = root.join(".config").join(brand::NAME);
        let data = root.join(".local").join("share").join(brand::NAME);
        KitDirs {
            home: root.to_path_buf(),
            config,
            bin: data.join("bin"),
            backups: data.join("backups"),
            data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// The running OS. Any other OS is an error, because v1 does not build for it.
pub fn current_os() -> Result<Os> {
    if cfg!(target_os = "linux") {
        Ok(Os::Linux)
    } else if cfg!(target_os = "macos") {
        Ok(Os::MacOs)
    } else {
        Err(anyhow!(
            "unsupported OS: {} is not supported",
            std::env::consts::OS
        ))
    }
}

/// The running CPU architecture. Any other architecture is an error.
pub fn current_arch() -> Result<Arch> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(Arch::X86_64),
        "aarch64" => Ok(Arch::Aarch64),
        other => Err(anyhow!("unsupported architecture: {other}")),
    }
}

/// True when `path` is a regular file with at least one execute bit set.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kit_dirs_rooted_layout() {
        let dirs = KitDirs::rooted(Path::new("/h"));
        assert_eq!(dirs.home, PathBuf::from("/h"));
        assert_eq!(dirs.config, PathBuf::from("/h/.config/relaxkit"));
        assert_eq!(dirs.data, PathBuf::from("/h/.local/share/relaxkit"));
        assert_eq!(dirs.bin, PathBuf::from("/h/.local/share/relaxkit/bin"));
        assert_eq!(
            dirs.backups,
            PathBuf::from("/h/.local/share/relaxkit/backups")
        );
    }

    #[test]
    fn current_os_is_supported_here() {
        assert!(current_os().is_ok());
        assert!(current_arch().is_ok());
    }
}
