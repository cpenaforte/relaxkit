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
    Ok(chromium_candidates_for(current_os()?, current_arch()?))
}

/// The same paths for a given OS and CPU. Playwright unpacks Linux x86_64 to `chrome-linux64` and Linux arm64 to
/// `chrome-linux-arm64`; `chrome-linux` is the layout of older Playwright releases.
pub fn chromium_candidates_for(os: Os, arch: Arch) -> Vec<&'static str> {
    match (os, arch) {
        (Os::MacOs, _) => vec![
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ],
        (Os::Linux, Arch::X86_64) => vec!["chrome-linux64/chrome", "chrome-linux/chrome"],
        (Os::Linux, Arch::Aarch64) => vec!["chrome-linux-arm64/chrome", "chrome-linux/chrome"],
    }
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

/// What preparing Linux for the sandbox takes (spec 7). `lines` is shown to the user before the one sudo prompt, and
/// `sudo` is that one invocation (`sudo` first). Both are empty when nothing is needed, which is every macOS plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrereqPlan {
    pub lines: Vec<String>,
    pub sudo: Vec<String>,
}

/// The AppArmor profile that lets bubblewrap create user namespaces on Ubuntu 24.04 and later. The Linux sandbox
/// needs it, because that release restricts unprivileged user namespaces by default (spec 7; spike B4).
pub const BWRAP_PROFILE_PATH: &str = "/etc/apparmor.d/relaxkit-bwrap";
const BWRAP_PROFILE: &str = "abi <abi/4.0>,\ninclude <tunables/global>\nprofile relaxkit-bwrap /usr/bin/bwrap flags=(unconfined) {\n  userns,\n}\n";

/// Where the AppArmor profile is looked for. `RELAXKIT_APPARMOR_PROFILE` replaces the path, so the end-to-end test can
/// stand a file in for the profile; an empty value is no override.
fn profile_path_from(override_value: Option<std::ffi::OsString>) -> PathBuf {
    match override_value.filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(BWRAP_PROFILE_PATH),
    }
}

/// Whether the AppArmor profile file is on disk. Read-only: relaxkit never writes it outside the sudo plan.
pub fn apparmor_profile_present() -> bool {
    profile_path_from(std::env::var_os("RELAXKIT_APPARMOR_PROFILE")).exists()
}

/// Whether a plan writes the AppArmor profile, which only Ubuntu 24.04 and later need (spec 7).
pub fn plan_needs_apparmor_profile(plan: &PrereqPlan) -> bool {
    plan.sudo.iter().any(|arg| arg.contains(BWRAP_PROFILE_PATH))
}

/// The plan for `os`. `os_release` is the text of `/etc/os-release` on Linux. Nothing here runs a command.
pub fn prerequisite_plan(os: Os, os_release: Option<&str>) -> PrereqPlan {
    if os != Os::Linux {
        return PrereqPlan::default();
    }
    let mut lines = vec!["install bubblewrap and socat with apt-get".to_string()];
    let mut script = "apt-get update && apt-get install -y bubblewrap socat".to_string();
    if os_release.is_some_and(needs_userns_profile) {
        lines.push(format!(
            "write the AppArmor profile {BWRAP_PROFILE_PATH} so bubblewrap may create user namespaces, then load it"
        ));
        script.push_str(&format!(
            " && printf '%s' '{BWRAP_PROFILE}' > {BWRAP_PROFILE_PATH} && apparmor_parser -r {BWRAP_PROFILE_PATH}"
        ));
    }
    PrereqPlan {
        lines,
        sudo: vec![
            "sudo".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            script,
        ],
    }
}

/// True for Ubuntu 24.04 or later, read from `/etc/os-release` text (`ID=ubuntu`, `VERSION_ID="24.04"`).
fn needs_userns_profile(os_release: &str) -> bool {
    let field = |key: &str| {
        os_release.lines().find_map(|line| {
            line.strip_prefix(key)?
                .strip_prefix('=')
                .map(|value| value.trim_matches('"').to_string())
        })
    };
    if field("ID").as_deref() != Some("ubuntu") {
        return false;
    }
    let Some(version) = field("VERSION_ID") else {
        return false;
    };
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    matches!(
        (parts.next().flatten(), parts.next().flatten()),
        (Some(major), Some(minor)) if (major, minor) >= (24, 4)
    )
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
    fn linux_chromium_is_found_in_the_layout_playwright_writes_for_each_cpu() {
        // Playwright unpacks Linux x86_64 to `chrome-linux64` and Linux arm64 to `chrome-linux-arm64`.
        assert_eq!(
            chromium_candidates_for(Os::Linux, Arch::X86_64)[0],
            "chrome-linux64/chrome"
        );
        assert!(
            chromium_candidates_for(Os::Linux, Arch::Aarch64)
                .contains(&"chrome-linux-arm64/chrome"),
            "{:?}",
            chromium_candidates_for(Os::Linux, Arch::Aarch64)
        );
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

    const UBUNTU_2404: &str = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";

    #[test]
    fn the_profile_path_is_the_default_unless_the_override_is_set() {
        assert_eq!(profile_path_from(None), PathBuf::from(BWRAP_PROFILE_PATH));
        assert_eq!(
            profile_path_from(Some("".into())),
            PathBuf::from(BWRAP_PROFILE_PATH),
            "an empty override is no override"
        );
        assert_eq!(
            profile_path_from(Some("/stand-in/relaxkit-bwrap".into())),
            PathBuf::from("/stand-in/relaxkit-bwrap")
        );
    }

    #[test]
    fn prerequisite_plan_on_macos_is_empty() {
        assert_eq!(prerequisite_plan(Os::MacOs, None), PrereqPlan::default());
    }

    #[test]
    fn prerequisite_plan_names_bubblewrap_and_socat_in_one_sudo_invocation() {
        let plan = prerequisite_plan(Os::Linux, Some("ID=debian\nVERSION_ID=\"12\"\n"));

        let sudo_words = plan.sudo.iter().filter(|word| *word == "sudo").count();
        assert_eq!(sudo_words, 1, "{:?}", plan.sudo);
        assert_eq!(plan.sudo.first().map(String::as_str), Some("sudo"));
        let script = plan.sudo.join(" ");
        assert!(script.contains("bubblewrap"), "{script}");
        assert!(script.contains("socat"), "{script}");
        assert!(!script.contains("apparmor_parser"), "{script}");
    }

    #[test]
    fn prerequisite_plan_refreshes_the_apt_index_before_installing_in_the_same_sudo_call() {
        // A fresh image has a stale index, so the install fails without an update first (spec 4 and 7: one prompt).
        let plan = prerequisite_plan(Os::Linux, None);

        assert_eq!(plan.sudo.iter().filter(|word| *word == "sudo").count(), 1);
        assert_eq!(plan.sudo[..3], ["sudo", "sh", "-c"]);
        let script = &plan.sudo[3];
        let update = script.find("apt-get update").expect("an apt-get update");
        let install = script.find("apt-get install").expect("an apt-get install");
        assert!(update < install, "{script}");
    }

    #[test]
    fn prerequisite_plan_adds_the_apparmor_profile_on_ubuntu_2404_and_later() {
        for release in [
            UBUNTU_2404,
            "ID=ubuntu\nVERSION_ID=\"24.10\"\n",
            "ID=ubuntu\nVERSION_ID=\"26.04\"\n",
        ] {
            let plan = prerequisite_plan(Os::Linux, Some(release));
            let script = plan.sudo.join(" ");
            assert!(script.contains("apparmor_parser"), "{release}: {script}");
            assert!(script.contains("userns"), "{release}: {script}");
            assert_eq!(plan.sudo.iter().filter(|word| *word == "sudo").count(), 1);
        }
    }

    #[test]
    fn prerequisite_plan_skips_apparmor_before_ubuntu_2404_and_on_other_distros() {
        for release in [
            "ID=ubuntu\nVERSION_ID=\"22.04\"\n",
            "ID_LIKE=ubuntu\nVERSION_ID=\"24.04\"\n",
            "ID=debian\nVERSION_ID=\"13\"\n",
        ] {
            let plan = prerequisite_plan(Os::Linux, Some(release));
            assert!(
                !plan.sudo.join(" ").contains("apparmor_parser"),
                "{release}"
            );
        }
    }
}
