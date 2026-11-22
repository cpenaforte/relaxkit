//! Self-update of relaxkit itself (Task 22, rulings K91 to K100). A verified release is downloaded: its checksums file
//! is checked against the embedded minisign key, then the archive is checked against that file, and only then is the
//! binary swapped. Nothing is written before both checks pass. A Homebrew-owned binary is left to `brew upgrade`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use minisign_verify::{PublicKey, Signature};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::fsx::write_atomic;
use crate::net::github_latest_tag;
use crate::runtime::{checksum_for, download_dir, fresh_dir, sha256_hex, utf8};
use crate::state::record_backup;

/// The GitHub repository whose latest release is the newest relaxkit (K97). A placeholder until the owner confirms it.
pub const RELEASE_REPO: &str = "OWNER/relaxkit";

/// The minisign public key embedded in the binary (K91). Until the owner creates the real key, this is a placeholder.
pub const EMBEDDED_KEY: &str = include_str!("../release/minisign.pub");

/// The text that marks the embedded key as a placeholder (K91).
pub const PLACEHOLDER_MARKER: &str = "relaxkit-placeholder";

/// The version this binary was built as.
const INSTALLED: &str = env!("CARGO_PKG_VERSION");

/// The outcome of the self step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfUpdate {
    /// Installed is the newest release (or newer); nothing was downloaded.
    Current,
    /// A check or a dry run found a newer release. Nothing changed.
    Available { version: String },
    /// Swapped to `to`, and the new binary's `update --tools` ran and exited zero (K96).
    Updated { from: String, to: String },
    /// A Homebrew-owned binary was updated by `brew upgrade relaxkit` (K93).
    DelegatedToBrew,
    /// The embedded key is the placeholder, so nothing was fetched or swapped (K91).
    NotConfigured,
    /// A check could not reach the release repository, so the newest version is unknown (K98).
    NewestUnknown { reason: String },
}

/// The self step for the running binary, with the embedded key. The running path is resolved through symbolic links
/// first, so a run as `rlx` replaces the real file (K95).
pub fn run(ctx: &Ctx, check_only: bool) -> Result<SelfUpdate> {
    let invoked = std::env::current_exe().context("cannot find the running relaxkit")?;
    let exe = resolve_exe(&invoked)?;
    apply(ctx, &exe, EMBEDDED_KEY, check_only)
}

/// The running binary's path with every symbolic link followed.
pub fn resolve_exe(invoked: &Path) -> Result<PathBuf> {
    fs::canonicalize(invoked).with_context(|| format!("cannot resolve {}", invoked.display()))
}

/// The self step for an already resolved binary path `exe`, verifying with `pubkey_text`. `exe` must not be a link.
/// A check, or a dry run, changes nothing and downloads nothing beyond the release lookup.
pub fn apply(ctx: &Ctx, exe: &Path, pubkey_text: &str, check_only: bool) -> Result<SelfUpdate> {
    if pubkey_text.contains(PLACEHOLDER_MARKER) {
        return Ok(SelfUpdate::NotConfigured);
    }
    let act = !check_only && !ctx.dry_run;
    if act && is_homebrew_path(exe) {
        let out = ctx
            .runner
            .run(&Cmd::new("brew").args(&["upgrade", "relaxkit"]))?;
        if out.code != 0 {
            bail!(
                "brew upgrade relaxkit exited {}: {}",
                out.code,
                out.stderr.trim()
            );
        }
        return Ok(SelfUpdate::DelegatedToBrew);
    }
    let newest = match newest_version(ctx) {
        Ok(version) => version,
        Err(err) if !act => {
            return Ok(SelfUpdate::NewestUnknown {
                reason: format!("{err:#}"),
            });
        }
        Err(err) => return Err(err),
    };
    if !is_newer(&newest, INSTALLED)? {
        return Ok(SelfUpdate::Current);
    }
    if !act {
        return Ok(SelfUpdate::Available { version: newest });
    }
    install(ctx, exe, pubkey_text, &newest)?;
    rerun(ctx, exe)?;
    Ok(SelfUpdate::Updated {
        from: INSTALLED.to_string(),
        to: newest,
    })
}

/// Checks a minisign signature over `bytes` with the public key in `pubkey_text` (R11). The signature must be the
/// prehashed (`ED`) form that minisign writes by default.
pub fn verify(bytes: &[u8], sig_text: &str, pubkey_text: &str) -> Result<()> {
    let key_line = pubkey_text
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("untrusted comment:"))
        .ok_or_else(|| anyhow!("the public key file holds no key"))?;
    let key = PublicKey::from_base64(key_line.trim())
        .map_err(|err| anyhow!("the public key is not a minisign key: {err}"))?;
    let signature = Signature::decode(sig_text)
        .map_err(|err| anyhow!("the signature is not a minisign signature: {err}"))?;
    key.verify(bytes, &signature, false)
        .map_err(|err| anyhow!("the release signature does not verify: {err}"))
}

/// The release target triple for an OS and architecture as `std::env::consts` names them, or `None` when no release
/// exists for it (K94).
pub fn triple_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// The release target of this build (K94). The OS and architecture are compile-time constants.
pub fn target() -> Result<&'static str> {
    triple_for(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "there is no relaxkit release for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })
}

/// The archive name of a release: `relaxkit-<version>-<target>.tar.gz`.
pub fn asset_name(version: &str, target: &str) -> String {
    format!("relaxkit-{version}-{target}.tar.gz")
}

/// Whether a path is inside a Homebrew Cellar, so brew owns the binary (K93).
pub fn is_homebrew_path(path: &Path) -> bool {
    [
        "/opt/homebrew/Cellar",
        "/usr/local/Cellar",
        "/home/linuxbrew",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

/// The text row for the self step, shown before the tool phase.
pub fn row(outcome: &SelfUpdate) -> String {
    match outcome {
        SelfUpdate::Current => format!("relaxkit {INSTALLED}: current"),
        SelfUpdate::Available { version } => {
            format!("relaxkit {INSTALLED} -> {version}: update available")
        }
        SelfUpdate::Updated { from, to } => format!("relaxkit {from} -> {to}: updated"),
        SelfUpdate::DelegatedToBrew => "relaxkit: updated by brew upgrade relaxkit".to_string(),
        SelfUpdate::NotConfigured => {
            "relaxkit: the release signing key is not configured, so the self update did nothing"
                .to_string()
        }
        SelfUpdate::NewestUnknown { reason } => {
            format!("relaxkit {INSTALLED} -> newest unknown: {reason}")
        }
    }
}

/// The newest release of relaxkit, without its leading `v`, from the latest GitHub release (K97).
fn newest_version(ctx: &Ctx) -> Result<String> {
    let tag = github_latest_tag(ctx.net.as_ref(), RELEASE_REPO)?;
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| anyhow!("release tag {tag} does not start with v"))?;
    semver::Version::parse(version)
        .with_context(|| format!("release tag {tag} is not a version"))?;
    Ok(version.to_string())
}

/// Whether `newest` is a later version than `installed`. An older tag never downgrades.
fn is_newer(newest: &str, installed: &str) -> Result<bool> {
    let newest =
        semver::Version::parse(newest).with_context(|| format!("{newest} is not a version"))?;
    let installed = semver::Version::parse(installed)
        .with_context(|| format!("{installed} is not a version"))?;
    Ok(newest > installed)
}

/// Downloads, verifies and unpacks the release, then swaps the binary. The checksums file is verified before the
/// archive is downloaded, and the archive before it is unpacked. Any failure before the swap leaves `exe` unchanged.
fn install(ctx: &Ctx, exe: &Path, pubkey_text: &str, version: &str) -> Result<()> {
    let target = target()?;
    let base = format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}");
    let checksums_name = format!("relaxkit-{version}-checksums.txt");
    let checksums = ctx.net.get_text(&format!("{base}/{checksums_name}"))?;
    let signature = ctx
        .net
        .get_text(&format!("{base}/{checksums_name}.minisig"))?;
    verify(checksums.as_bytes(), &signature, pubkey_text)?;

    let asset = asset_name(version, target);
    let expected = checksum_for(&checksums, &asset)?;
    let work = download_dir(ctx, &ctx.run, "selfupdate");
    fresh_dir(&work)?;
    let archive = work.join(&asset);
    ctx.net.download(&format!("{base}/{asset}"), &archive)?;
    let bytes = fs::read(&archive).with_context(|| format!("cannot read {}", archive.display()))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "checksum mismatch for {asset}: the release lists {expected}, the download is {actual}"
        );
    }

    let extract = work.join("extract");
    fs::create_dir_all(&extract).with_context(|| format!("cannot create {}", extract.display()))?;
    let out =
        ctx.runner
            .run(&Cmd::new("tar").args(&["-xf", utf8(&archive)?, "-C", utf8(&extract)?]))?;
    if out.code != 0 {
        bail!(
            "cannot unpack {asset} (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    let body = fs::read(extract.join("relaxkit"))
        .with_context(|| format!("{asset} has no relaxkit binary"))?;
    swap(ctx, exe, &body)
}

/// Replaces `exe` with `body`. The previous binary is kept at `dirs.bin/relaxkit.prev`, and both files are journaled
/// before they are written, so undo can put them back. The new binary is staged in `dirs.bin` and renamed over `exe`,
/// which is atomic, so a failed swap leaves the installed binary unchanged.
fn swap(ctx: &Ctx, exe: &Path, body: &[u8]) -> Result<()> {
    let bin = &ctx.dirs.bin;
    fs::create_dir_all(bin).with_context(|| format!("cannot create {}", bin.display()))?;
    let current = fs::read(exe).with_context(|| format!("cannot read {}", exe.display()))?;
    let prev = bin.join("relaxkit.prev");
    let staged = bin.join("relaxkit.new");
    // A link at either path, or at the temp name write_atomic creates beside it, would be written through. All four are
    // refused before anything is journaled or written.
    for path in [&prev, &staged] {
        refuse_link(path)?;
        refuse_link(&path.with_file_name(format!(
            "{}.relaxkit-tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        )))?;
    }

    record_backup(ctx, &ctx.run, &prev)?;
    write_executable(&prev, &current)?;
    record_backup(ctx, &ctx.run, exe)?;
    write_executable(&staged, body)?;
    if let Err(err) = fs::rename(&staged, exe) {
        let _ = fs::remove_file(&staged);
        return Err(err).with_context(|| {
            format!(
                "cannot replace {}; the installed relaxkit is unchanged",
                exe.display()
            )
        });
    }
    Ok(())
}

/// Refuses a path that is a symbolic link, since a write there would reach the link's target.
fn refuse_link(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        bail!(
            "{} is a link; relaxkit does not write through it",
            path.display()
        );
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically, with mode 0755.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("cannot make {} executable", path.display()))
}

/// Runs the new binary as `<new relaxkit> update --tools`, so the tool phase uses the new recipes (K96). Its output is
/// passed on, and a non-zero exit is an error.
fn rerun(ctx: &Ctx, exe: &Path) -> Result<()> {
    let program = utf8(exe)?;
    let out = ctx
        .runner
        .run(&Cmd::new(program).args(&["update", "--tools"]))
        .with_context(|| format!("the new relaxkit at {} did not run", exe.display()))?;
    for line in out.stdout.lines() {
        ctx.out.info(line);
    }
    for line in out.stderr.lines() {
        ctx.out.warn(line);
    }
    if out.code != 0 {
        bail!(
            "the new relaxkit's tool phase exited {}: {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(())
}
