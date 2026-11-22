//! gh: an existing gh is adopted and never changed. Otherwise a private copy of the official release goes
//! into the pack bin directory, after its archive matches the release's checksums file. Never `sudo`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::fsx::write_atomic;
use crate::net::{Net, github_latest_tag};
use crate::platform::{Arch, Os, current_arch, current_os, is_executable};
use crate::runtime::{checksum_for, download_dir, fresh_dir, sha256_hex, utf8};
use crate::state::RunId;
use crate::state::lockfile::{LockedTool, Lockfile, check_version};
use crate::tools::engine::UNVERSIONED;

/// The repository whose release archive is installed.
pub const GH_REPO: &str = "cli/cli";

/// The gh that a command will run. `owned_by_pack` is true only for the copy relaxkit installed,
/// so only that copy is ever updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhInstall {
    pub path: PathBuf,
    pub owned_by_pack: bool,
}

/// The lock key and route of the gh pack copy (K84).
pub const GH_ID: &str = "gh";
const GH_ROUTE: &str = "release";

/// Records the pack copy in the lock when relaxkit installed it (K84). A brew or system gh is never recorded, and a dry
/// run writes nothing. The version is the one `gh --version` reports, or `unversioned` when none is read.
pub fn record_pack_copy(ctx: &Ctx, run: &RunId, gh: &GhInstall) -> Result<()> {
    if ctx.dry_run || !gh.owned_by_pack || gh.path != ctx.dirs.bin.join("gh") {
        return Ok(());
    }
    // A link is never the pack copy: its target belongs to another install (K84, F7-1).
    if is_link(&gh.path) {
        return Ok(());
    }
    // The lock takes the version the copy reports, replacing an older entry (F7-3). A copy whose version cannot be read
    // keeps the entry it has, so a failed probe does not turn a known version into unversioned.
    let version = reported_version(ctx, &gh.path);
    let existing = Lockfile::load(ctx)?
        .and_then(|lock| lock.tools.get(GH_ID).map(|entry| entry.version.clone()));
    if existing.as_deref() == Some(version.as_str()) {
        return Ok(());
    }
    if version == UNVERSIONED && existing.is_some() {
        return Ok(());
    }
    record_version(ctx, run, &version)
}

/// Whether `path` is a symbolic link (the link itself, not its target).
pub fn is_link(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Writes the gh entry for `version`, the release relaxkit placed in the pack bin directory.
pub fn record_version(ctx: &Ctx, run: &RunId, version: &str) -> Result<()> {
    let mut lock = Lockfile::load(ctx)?.unwrap_or_else(|| Lockfile {
        relaxkit: env!("CARGO_PKG_VERSION").to_string(),
        tools: BTreeMap::new(),
    });
    lock.tools.insert(
        GH_ID.to_string(),
        LockedTool {
            version: version.to_string(),
            route: GH_ROUTE.to_string(),
            hosts: Vec::new(),
            host_versions: BTreeMap::new(),
        },
    );
    lock.save(ctx, run)
}

/// The version `gh --version` reports for the copy at `path`, or `unversioned` when none is read.
fn reported_version(ctx: &Ctx, path: &Path) -> String {
    let cmd = Cmd::new(&path.display().to_string()).arg("--version");
    match ctx.runner.run(&cmd) {
        Ok(out) if out.code == 0 => {
            parse_version(&out.stdout).unwrap_or_else(|| UNVERSIONED.to_string())
        }
        _ => UNVERSIONED.to_string(),
    }
}

/// The first exact version after the word `version` in `gh --version` output, without a leading `v`.
pub fn parse_version(text: &str) -> Option<String> {
    let token = text
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)?;
    let version = token.strip_prefix('v').unwrap_or(token);
    check_version(version).ok().map(|_| version.to_string())
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
    // A dangling link, or a link to a file that is not executable, is where an install would write through it: it is
    // refused before any download or write (K84, F8-1). A link to an executable gh is used as present above (K90).
    if is_link(&bin) {
        bail!(
            "the pack path {} is a link; relaxkit does not write through it",
            bin.display()
        );
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
    let fetched = fetch_verified(ctx, run, os, arch)?;
    place(ctx, &fetched.body)
}

/// The newest release, verified and unpacked, ready to place. Nothing is written into the pack bin directory.
pub struct Fetched {
    pub version: String,
    pub body: Vec<u8>,
}

/// The newest release of gh for this platform, verified against its checksums (K84).
pub fn fetch_latest(ctx: &Ctx, run: &RunId) -> Result<Fetched> {
    fetch_verified(ctx, run, current_os()?, current_arch()?)
}

/// The newest gh release, without its leading `v`.
pub fn latest_version(net: &dyn Net) -> Result<String> {
    let tag = github_latest_tag(net, GH_REPO)?;
    tag.strip_prefix('v')
        .map(str::to_string)
        .ok_or_else(|| anyhow!("release tag {tag} does not start with v"))
}

/// Writes `body` as the pack copy of gh, executable.
pub fn place(ctx: &Ctx, body: &[u8]) -> Result<()> {
    fs::create_dir_all(&ctx.dirs.bin)
        .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
    let bin = ctx.dirs.bin.join("gh");
    write_atomic(&bin, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot make {} executable", bin.display()))?;
    }
    Ok(())
}

/// Runs `gh --version` on the copy at `path` and checks that it reports `version`.
pub fn probe(ctx: &Ctx, path: &Path, version: &str) -> Result<()> {
    let cmd = Cmd::new(&path.display().to_string()).arg("--version");
    let out = ctx
        .runner
        .run(&cmd)
        .with_context(|| format!("the new gh at {} did not run", path.display()))?;
    if out.code != 0 || parse_version(&out.stdout).as_deref() != Some(version) {
        bail!(
            "the new gh at {} did not report version {version}",
            path.display()
        );
    }
    Ok(())
}

fn fetch_verified(ctx: &Ctx, run: &RunId, os: Os, arch: Arch) -> Result<Fetched> {
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
    Ok(Fetched {
        version: version.to_string(),
        body,
    })
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
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::platform::KitDirs;
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
    fn a_link_to_a_working_gh_is_used_as_present_with_no_write_and_no_entry() {
        // The link points at an executable gh and dirs.bin is not on the PATH: it is used as the present copy, with no
        // download, no write, and no lock entry (K90, F9-1).
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("elsewhere").join("gh");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "#!/bin/sh\necho gh\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let bin = KitDirs::rooted(root.path()).bin;
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&target, bin.join("gh")).unwrap();
        let before_mode = fs::metadata(&target).unwrap().permissions().mode();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let run = RunId::new();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert_eq!(
            gh,
            GhInstall {
                path: bin.join("gh"),
                owned_by_pack: true,
            }
        );
        assert!(
            runner.lines().iter().all(|line| !line.starts_with("tar")),
            "{:?}",
            runner.lines()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "#!/bin/sh\necho gh\n");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode(),
            before_mode,
            "the target's mode changed"
        );
        assert!(
            lock_in(&ctx).is_none(),
            "a link to a working gh was recorded"
        );
    }

    #[test]
    fn a_dangling_link_at_the_pack_path_is_refused_before_any_download() {
        // The install would write through the link into a path that does not exist, so it is refused (K90, F9-1).
        let root = tempfile::tempdir().unwrap();
        let bin = KitDirs::rooted(root.path()).bin;
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(root.path().join("missing").join("gh"), bin.join("gh")).unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        let err = ensure_gh_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap_err();

        assert!(
            format!("{err:#}").contains("is a link; relaxkit does not write through it"),
            "{err:#}"
        );
        assert!(
            runner.lines().iter().all(|line| !line.starts_with("tar")),
            "{:?}",
            runner.lines()
        );
    }

    #[test]
    fn a_link_at_the_pack_path_stops_the_install_and_is_never_written_through() {
        // The target is a regular file that is not executable, so the install path would be reached. The link is
        // refused before any download or write: the target keeps its bytes and mode, and no lock entry is written
        // (K84, F8-1).
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("elsewhere").join("gh");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "elsewhere bytes\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let bin = KitDirs::rooted(root.path()).bin;
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&target, bin.join("gh")).unwrap();
        let before_mode = fs::metadata(&target).unwrap().permissions().mode();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let run = RunId::new();

        let err = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap_err();

        assert!(
            format!("{err:#}").contains("is a link; relaxkit does not write through it"),
            "{err:#}"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "elsewhere bytes\n");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode(),
            before_mode,
            "the target's mode changed"
        );
        assert!(
            runner.lines().iter().all(|line| !line.starts_with("tar")),
            "{:?}",
            runner.lines()
        );
        assert!(lock_in(&ctx).is_none(), "a link was recorded");
        // A link that reaches the record step is still never recorded (F7-1).
        let link = GhInstall {
            path: bin.join("gh"),
            owned_by_pack: true,
        };
        record_pack_copy(&ctx, &run, &link).unwrap();
        assert!(lock_in(&ctx).is_none(), "a link was recorded");
    }

    #[test]
    fn a_reinstalled_pack_copy_replaces_an_older_locked_version_with_the_one_it_reports() {
        // The lock says 2.60.0 but the copy is missing: setup installs the release tagged 9.9.9, and the copy reports
        // 9.9.8. The lock takes what the copy reports, not the tag (F7-3).
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = fixture(root.path(), None);
        let bin = KitDirs::rooted(root.path()).bin.join("gh");
        let runner =
            Recorder::new(version_reply(&bin, "gh version 9.9.8 (2026-01-01)\n")).real("tar -xf");
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));
        let run = RunId::new();
        record_version(&ctx, &run, "2.60.0").unwrap();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert_eq!(lock_in(&ctx).unwrap().tools["gh"].version, "9.9.8");
    }

    /// The lock file, if one was written.
    fn lock_in(ctx: &Ctx) -> Option<crate::state::lockfile::Lockfile> {
        crate::state::lockfile::Lockfile::load(ctx).unwrap()
    }

    /// An executable pack copy that is already in place (a rerun), without any download.
    fn existing_pack_copy(root: &Path) {
        let bin = KitDirs::rooted(root).bin;
        fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(&gh, "#!/bin/sh\necho gh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn version_reply(bin: &Path, text: &str) -> FakeRunner {
        FakeRunner::new().reply(&format!("{} --version", bin.display()), Output::ok(text))
    }

    #[test]
    fn a_fresh_pack_copy_is_recorded_with_the_version_it_reports() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = fixture(root.path(), None);
        let bin = KitDirs::rooted(root.path()).bin.join("gh");
        let runner =
            Recorder::new(version_reply(&bin, "gh version 9.9.9 (2026-01-01)\n")).real("tar -xf");
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));
        let run = RunId::new();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        let lock = lock_in(&ctx).expect("the install wrote no lock");
        assert_eq!(lock.tools["gh"].version, "9.9.9");
        assert_eq!(lock.tools["gh"].route, "release");
    }

    #[test]
    fn a_rerun_with_the_pack_copy_records_it_without_reinstalling() {
        let root = tempfile::tempdir().unwrap();
        existing_pack_copy(root.path());
        let bin = KitDirs::rooted(root.path()).bin.join("gh");
        let runner = Recorder::new(version_reply(&bin, "gh version 9.9.9 (2026-01-01)\n"));
        // No download is served: a reinstall would fail here.
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let run = RunId::new();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert!(
            runner.lines().iter().all(|line| !line.starts_with("tar")),
            "{:?}",
            runner.lines()
        );
        assert_eq!(lock_in(&ctx).unwrap().tools["gh"].version, "9.9.9");
    }

    #[test]
    fn a_brew_gh_is_never_recorded() {
        let root = tempfile::tempdir().unwrap();
        let runner =
            Recorder::new(FakeRunner::new().with_program("gh", Path::new("/opt/homebrew/bin/gh")));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let run = RunId::new();

        let gh = ensure_gh(&ctx, &run).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert!(
            lock_in(&ctx).is_none_or(|lock| !lock.tools.contains_key("gh")),
            "a brew gh was recorded"
        );
    }

    #[test]
    fn a_pack_copy_with_no_readable_version_is_recorded_as_unversioned() {
        let root = tempfile::tempdir().unwrap();
        existing_pack_copy(root.path());
        let bin = KitDirs::rooted(root.path()).bin.join("gh");
        let runner = Recorder::new(version_reply(&bin, "no version here\n"));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let run = RunId::new();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert_eq!(
            lock_in(&ctx).unwrap().tools["gh"].version,
            crate::tools::engine::UNVERSIONED
        );
    }

    #[test]
    fn a_dry_run_writes_no_lock_entry_for_the_pack_copy() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let mut ctx = ctx_with(root.path(), &runner, FakeNet::new());
        ctx.dry_run = true;
        let run = RunId::new();

        let gh = ensure_gh_for(&ctx, &run, Os::Linux, Arch::X86_64).unwrap();
        record_pack_copy(&ctx, &run, &gh).unwrap();

        assert!(lock_in(&ctx).is_none(), "a dry run wrote a lock");
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
