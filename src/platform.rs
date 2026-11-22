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

/// Where Playwright keeps its browsers unless `PLAYWRIGHT_BROWSERS_PATH` says otherwise.
pub fn playwright_cache_dir(home: &Path) -> Result<PathBuf> {
    Ok(match current_os()? {
        Os::MacOs => home.join("Library").join("Caches").join("ms-playwright"),
        Os::Linux => home.join(".cache").join("ms-playwright"),
    })
}

/// Paths of the Chromium executable inside one `chromium-<revision>` directory, for this OS and architecture.
/// The first one that exists is used.
pub fn chromium_candidates() -> Result<Vec<&'static str>> {
    Ok(match current_os()? {
        Os::MacOs => vec![
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ],
        Os::Linux => vec!["chrome-linux64/chrome", "chrome-linux/chrome"],
    })
}

/// True when `path` is a regular file with at least one execute bit set.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Where rtk keeps its `config.toml` under `home`. The macOS path is from spike A7; the Linux path follows the
/// XDG default and is to be verified at Task 28.
pub fn rtk_config_file(home: &Path) -> Result<PathBuf> {
    Ok(rtk_config_for(current_os()?, home))
}

fn rtk_config_for(os: Os, home: &Path) -> PathBuf {
    match os {
        Os::MacOs => home
            .join("Library")
            .join("Application Support")
            .join("rtk")
            .join("config.toml"),
        Os::Linux => home.join(".config").join("rtk").join("config.toml"),
    }
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
        #[cfg(target_os = "macos")]
        assert_eq!(current_os().unwrap(), Os::MacOs);
        #[cfg(target_os = "linux")]
        assert_eq!(current_os().unwrap(), Os::Linux);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert!(current_os().is_err());

        #[cfg(target_arch = "x86_64")]
        assert_eq!(current_arch().unwrap(), Arch::X86_64);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(current_arch().unwrap(), Arch::Aarch64);
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        assert!(current_arch().is_err());
    }

    #[test]
    fn rtk_config_path_follows_each_os() {
        let home = Path::new("/h");
        assert_eq!(
            rtk_config_for(Os::MacOs, home),
            PathBuf::from("/h/Library/Application Support/rtk/config.toml")
        );
        assert_eq!(
            rtk_config_for(Os::Linux, home),
            PathBuf::from("/h/.config/rtk/config.toml")
        );
    }
}
