//! gh: an existing gh is adopted and never changed. Otherwise a private copy of the official release goes
//! into the pack bin directory, after its archive matches the release's checksums file. Never `sudo`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::fsx::write_atomic;
use crate::net::github_latest_tag;
use crate::platform::{Arch, Os, current_arch, current_os, is_executable};
use crate::runtime::{checksum_for, download_dir, fresh_dir, sha256_hex, utf8};
use crate::state::RunId;

/// The repository whose release archive is installed.
pub const GH_REPO: &str = "cli/cli";

/// The gh that a command will run. `owned_by_pack` is true only for the copy relaxkit installed,
/// so only that copy is ever updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhInstall {
    pub path: PathBuf,
    pub owned_by_pack: bool,
}

pub fn ensure_gh(ctx: &Ctx, run: &RunId) -> Result<GhInstall> {
    ensure_gh_for(ctx, run, current_os()?, current_arch()?)
}

fn ensure_gh_for(ctx: &Ctx, run: &RunId, os: Os, arch: Arch) -> Result<GhInstall> {
    let bin = ctx.dirs.bin.join("gh");
    if let Some(path) = ctx.runner.which("gh") {
        let owned_by_pack = path == bin;
        return Ok(GhInstall {
            path,
            owned_by_pack,
        });
    }
    if is_executable(&bin) {
        return Ok(GhInstall {
            path: bin,
            owned_by_pack: true,
        });
    }
    if ctx.dry_run {
        ctx.out
            .info(&format!("would install gh into {}", ctx.dirs.bin.display()));
        return Ok(GhInstall {
            path: bin,
            owned_by_pack: true,
        });
    }
    install_private_copy(ctx, run, os, arch)?;
    Ok(GhInstall {
        path: bin,
        owned_by_pack: true,
    })
}

/// Downloads the release archive and its checksums into a fresh directory, checks the archive's SHA-256
/// against the checksums file, and only then unpacks it and copies `bin/gh` into the pack bin directory.
/// A mismatch stops before anything is unpacked or written.
fn install_private_copy(ctx: &Ctx, run: &RunId, os: Os, arch: Arch) -> Result<()> {
    let tag = github_latest_tag(ctx.net.as_ref(), GH_REPO)?;
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| anyhow!("release tag {tag} does not start with v"))?;
    let archive_name = asset_name(os, arch, version);
    let base = format!("https://github.com/{GH_REPO}/releases/download/{tag}");
    let work = download_dir(ctx, run, "gh");
    fresh_dir(&work)?;

    let checksums_path = work.join("checksums.txt");
    ctx.net.download(
        &format!("{base}/gh_{version}_checksums.txt"),
        &checksums_path,
    )?;
    let archive = work.join(&archive_name);
    ctx.net
        .download(&format!("{base}/{archive_name}"), &archive)?;

    let checksums = fs::read_to_string(&checksums_path)
        .with_context(|| format!("cannot read {}", checksums_path.display()))?;
    let expected = checksum_for(&checksums, &archive_name)?;
    let bytes = fs::read(&archive).with_context(|| format!("cannot read {}", archive.display()))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "checksum mismatch for {archive_name}: the release lists {expected}, the download is {actual}"
        );
    }

    let extract = work.join("extract");
    fs::create_dir_all(&extract).with_context(|| format!("cannot create {}", extract.display()))?;
    let out =
        ctx.runner
            .run(&Cmd::new("tar").args(&["-xf", utf8(&archive)?, "-C", utf8(&extract)?]))?;
    if out.code != 0 {
        bail!(
            "cannot unpack {archive_name} (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    let stem = archive_name
        .strip_suffix(".tar.gz")
        .or_else(|| archive_name.strip_suffix(".zip"))
        .ok_or_else(|| anyhow!("unexpected archive name {archive_name}"))?;
    let built = extract.join(stem).join("bin").join("gh");
    let body = fs::read(&built).with_context(|| format!("{archive_name} has no bin/gh"))?;

    fs::create_dir_all(&ctx.dirs.bin)
        .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
    let bin = ctx.dirs.bin.join("gh");
    write_atomic(&bin, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot make {} executable", bin.display()))?;
    }
    Ok(())
}

/// The release archive for `version` (no leading `v`), named as the release publishes it.
fn asset_name(os: Os, arch: Arch, version: &str) -> String {
    let os = match os {
        Os::Linux => "linux",
        Os::MacOs => "macOS",
    };
    let arch = match arch {
        Arch::X86_64 => "amd64",
        Arch::Aarch64 => "arm64",
    };
    let ext = match os {
        "macOS" => "zip",
        _ => "tar.gz",
    };
    format!("gh_{version}_{os}_{arch}.{ext}")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::runtime::testing::{Recorder, ctx_with};
    use crate::state::journal;

    const TAG_URL: &str = "https://api.github.com/repos/cli/cli/releases/latest";
    const STEM: &str = "gh_9.9.9_linux_amd64";
    const ARCHIVE: &str = "gh_9.9.9_linux_amd64.tar.gz";
    const CHECKSUMS: &str =
        "https://github.com/cli/cli/releases/download/v9.9.9/gh_9.9.9_checksums.txt";
    const ARCHIVE_URL: &str =
        "https://github.com/cli/cli/releases/download/v9.9.9/gh_9.9.9_linux_amd64.tar.gz";

    /// Builds a real `gh_<v>_linux_amd64.tar.gz` holding `bin/gh`, and its checksums file line.
    /// Returns the archive bytes and the checksums text.
    fn fixture(root: &Path, digest_override: Option<&str>) -> (Vec<u8>, String) {
        let staging = root.join("staging");
        let bin = staging.join(STEM).join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("gh"), "#!/bin/sh\necho gh\n").unwrap();
        let archive = root.join(ARCHIVE);
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .arg(STEM)
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = fs::read(&archive).unwrap();
        let digest = match digest_override {
            Some(text) => text.to_string(),
            None => sha256_of(&bytes),
        };
        (bytes, format!("{digest}  {ARCHIVE}\n"))
    }

    fn sha256_of(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn net(bytes: Vec<u8>, checksums: &str) -> FakeNet {
        FakeNet::new()
            .with(TAG_URL, r#"{"tag_name":"v9.9.9"}"#)
            .with(CHECKSUMS, checksums)
            .with_bytes(ARCHIVE_URL, bytes)
    }

    /// The system `tar` extracts for real. The archive is built in the test, not fetched.
    fn tar_runner() -> Recorder {
        Recorder::new(FakeRunner::new()).real("tar -xf")
    }

    #[test]
    fn existing_gh_is_adopted_and_marked_not_owned_by_pack() {
        let root = tempfile::tempdir().unwrap();
        let runner =
            Recorder::new(FakeRunner::new().with_program("gh", Path::new("/opt/homebrew/bin/gh")));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        let gh = ensure_gh(&ctx, &RunId::new()).unwrap();

        assert_eq!(
            gh,
            GhInstall {
                path: PathBuf::from("/opt/homebrew/bin/gh"),
                owned_by_pack: false,
            }
        );
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn missing_gh_installs_a_private_copy_into_the_pack_bin_dir() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = fixture(root.path(), None);
        let runner = tar_runner();
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));

        let gh = ensure_gh_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap();

        let bin_gh = ctx.dirs.bin.join("gh");
        assert_eq!(
            gh,
            GhInstall {
                path: bin_gh.clone(),
                owned_by_pack: true,
            }
        );
        assert_eq!(fs::read_to_string(&bin_gh).unwrap(), "#!/bin/sh\necho gh\n");
        let mode = fs::metadata(&bin_gh).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o755);
        assert!(
            runner.lines().iter().all(|line| !line.contains("sudo")),
            "{:?}",
            runner.lines()
        );
        assert!(
            journal::read(&ctx.dirs).unwrap().is_empty(),
            "a gh install edits no user file"
        );
    }

    #[test]
    fn gh_checksum_mismatch_errors_and_leaves_nothing_in_bin() {
        let root = tempfile::tempdir().unwrap();
        let wrong = "0".repeat(64);
        let (bytes, checksums) = fixture(root.path(), Some(&wrong));
        let runner = tar_runner();
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));

        let result = ensure_gh_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64);

        assert!(result.is_err());
        assert!(!ctx.dirs.bin.join("gh").exists());
        assert!(
            runner.lines().iter().all(|line| !line.starts_with("tar")),
            "an unverified archive was extracted: {:?}",
            runner.lines()
        );
    }

    #[test]
    fn gh_asset_names_follow_the_release_layout() {
        assert_eq!(
            asset_name(Os::Linux, Arch::X86_64, "9.9.9"),
            "gh_9.9.9_linux_amd64.tar.gz"
        );
        assert_eq!(
            asset_name(Os::MacOs, Arch::Aarch64, "9.9.9"),
            "gh_9.9.9_macOS_arm64.zip"
        );
    }

    #[test]
    fn checksum_line_is_found_by_file_name() {
        let text = format!(
            "{}  other.tar.gz\n{}  {ARCHIVE}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(checksum_for(&text, ARCHIVE).unwrap(), "b".repeat(64));
        assert!(checksum_for(&text, "missing.tar.gz").is_err());
    }
}
