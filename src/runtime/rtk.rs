//! rtk: adopted when it is on the PATH. Otherwise the latest release is installed into the pack bin directory. The
//! release archive for this platform is downloaded, its SHA-256 is checked against the same release's
//! `checksums.txt`, and only then is it unpacked. rtk's install script is never run, and Homebrew is never used.
//! Documented limit (spec section 11): rtk's archives carry no attestation and no signature, so a compromised
//! release would pass this check. The owner accepted that for rtk alone, because rtk is always the latest release.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::edit::toml_set;
use crate::exec::Cmd;
use crate::fsx::write_atomic;
use crate::net::github_latest_tag;
use crate::platform::{Arch, Os, current_arch, current_os, is_executable, rtk_config_file};
use crate::runtime::{checksum_for, download_dir, fresh_dir, sha256_hex, utf8};
use crate::state::{RunId, safe_write};

/// The repository whose latest release is installed.
pub const RTK_REPO: &str = "rtk-ai/rtk";

/// The rtk binary to use: the one on the PATH, else the pack's copy, else the latest release, installed there.
pub fn ensure_rtk(ctx: &Ctx, run: &RunId) -> Result<PathBuf> {
    ensure_rtk_for(ctx, run, current_os()?, current_arch()?)
}

fn ensure_rtk_for(ctx: &Ctx, run: &RunId, os: Os, arch: Arch) -> Result<PathBuf> {
    if let Some(path) = ctx.runner.which("rtk") {
        return Ok(path);
    }
    let bin = ctx.dirs.bin.join("rtk");
    if is_executable(&bin) {
        return Ok(bin);
    }
    if ctx.dry_run {
        ctx.out.info(&format!(
            "would install the latest rtk into {}",
            ctx.dirs.bin.display()
        ));
        return Ok(bin);
    }
    install_latest(ctx, run, os, arch)?;
    Ok(bin)
}

/// Downloads the latest release's archive for this platform and its checksums into a fresh directory, checks the
/// archive's SHA-256 against the checksums, and only then unpacks it and copies its `rtk` into the pack bin
/// directory. A mismatch stops before anything is unpacked or written.
fn install_latest(ctx: &Ctx, run: &RunId, os: Os, arch: Arch) -> Result<()> {
    let tag = github_latest_tag(ctx.net.as_ref(), RTK_REPO)?;
    if !tag_is_plain(&tag) {
        bail!(
            "release tag `{tag}` has characters that do not belong in a download URL; nothing was downloaded"
        );
    }
    let archive_name = asset_name(os, arch);
    let base = format!("https://github.com/{RTK_REPO}/releases/download/{tag}");
    let work = download_dir(ctx, run, "rtk");
    fresh_dir(&work)?;

    let checksums_path = work.join("checksums.txt");
    ctx.net
        .download(&format!("{base}/checksums.txt"), &checksums_path)?;
    let archive = work.join(archive_name);
    ctx.net
        .download(&format!("{base}/{archive_name}"), &archive)?;

    let checksums = fs::read_to_string(&checksums_path)
        .with_context(|| format!("cannot read {}", checksums_path.display()))?;
    let expected = checksum_for(&checksums, archive_name)?;
    let bytes = fs::read(&archive).with_context(|| format!("cannot read {}", archive.display()))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "checksum mismatch for {archive_name}: release {tag} lists {expected}, the download is {actual}; nothing was unpacked"
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
    let built = find_binary(&extract)?.ok_or_else(|| {
        anyhow!("{archive_name} has no rtk binary at its top level or in one folder")
    })?;
    let body = fs::read(&built).with_context(|| format!("cannot read {}", built.display()))?;

    fs::create_dir_all(&ctx.dirs.bin)
        .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
    let bin = ctx.dirs.bin.join("rtk");
    write_atomic(&bin, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("cannot make {} executable", bin.display()))?;
    }
    Ok(())
}

/// A release tag made of letters, digits and `.`, `-`, `_`, `+` only, so it cannot change the download path.
pub(crate) fn tag_is_plain(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// The `rtk` file at the top of the unpacked archive, or one folder down. Two matches would be ambiguous and are
/// refused.
fn find_binary(extract: &Path) -> Result<Option<PathBuf>> {
    let top = extract.join("rtk");
    if top.is_file() {
        return Ok(Some(top));
    }
    let mut found = Vec::new();
    for entry in
        fs::read_dir(extract).with_context(|| format!("cannot read {}", extract.display()))?
    {
        let candidate = entry?.path().join("rtk");
        if candidate.is_file() {
            found.push(candidate);
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => bail!("the archive holds more than one rtk binary"),
    }
}

/// The release archive for this platform, as the release publishes it. Linux uses the static musl build on both
/// architectures. The names are checked against the real release at Task 28.
fn asset_name(os: Os, arch: Arch) -> &'static str {
    match (os, arch) {
        (Os::MacOs, Arch::Aarch64) => "rtk-aarch64-apple-darwin.tar.gz",
        (Os::MacOs, Arch::X86_64) => "rtk-x86_64-apple-darwin.tar.gz",
        (Os::Linux, Arch::X86_64) => "rtk-x86_64-unknown-linux-musl.tar.gz",
        (Os::Linux, Arch::Aarch64) => "rtk-aarch64-unknown-linux-musl.tar.gz",
    }
}

/// Sets rtk's `[display] pro_hint = false`, which hides the one-line RTK Pro notice that `rtk gain` prints about
/// once a day (spec section 6, item 20: a proposal the owner may veto). Comments and other keys are kept.
pub fn apply_defaults(ctx: &Ctx, run: &RunId) -> Result<()> {
    let path = rtk_config_file(&ctx.dirs.home)?;
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let text = toml_set(&old, &["display", "pro_hint"], toml_edit::value(false))?;
    if text == old {
        return Ok(());
    }
    safe_write(ctx, run, &path, &text)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::runtime::testing::{Recorder, ctx_with};

    const TAG_URL: &str = "https://api.github.com/repos/rtk-ai/rtk/releases/latest";
    const CHECKSUMS: &str = "https://github.com/rtk-ai/rtk/releases/download/v9.9.9/checksums.txt";
    const ARCHIVE: &str = "rtk-x86_64-unknown-linux-musl.tar.gz";
    const ARCHIVE_URL: &str = "https://github.com/rtk-ai/rtk/releases/download/v9.9.9/rtk-x86_64-unknown-linux-musl.tar.gz";

    /// A real archive, built with the system tar, that holds `inner` (`rtk`, or `folder/rtk`) with a shell body.
    /// Returns the archive bytes and the checksums line that names it. `digest` overrides the true hash.
    fn release(root: &Path, name: &str, inner: &str, digest: Option<&str>) -> (Vec<u8>, String) {
        let staging = root.join("staging");
        let file = staging.join(inner);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "#!/bin/sh\necho rtk\n").unwrap();
        let archive = root.join(name);
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = fs::read(&archive).unwrap();
        let digest = digest
            .map(str::to_string)
            .unwrap_or_else(|| sha256_hex(&bytes));
        (bytes, format!("{digest}  {name}\n"))
    }

    fn net(bytes: Vec<u8>, checksums: &str) -> FakeNet {
        FakeNet::new()
            .with(TAG_URL, r#"{"tag_name":"v9.9.9"}"#)
            .with(CHECKSUMS, checksums)
            .with_bytes(ARCHIVE_URL, bytes)
    }

    /// The system `tar` extracts for real. Nothing else runs, so an installer or Homebrew would show up as a call.
    fn tar_runner() -> Recorder {
        Recorder::new(FakeRunner::new()).real("tar -xf")
    }

    fn bin_rtk(root: &Path) -> PathBuf {
        root.join(".local/share/relaxkit/bin/rtk")
    }

    #[test]
    fn rtk_on_the_path_is_adopted() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            FakeRunner::new().with_program("rtk", Path::new("/opt/homebrew/bin/rtk")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        let path = ensure_rtk_for(&ctx, &RunId::new(), Os::MacOs, Arch::Aarch64).unwrap();

        assert_eq!(path, PathBuf::from("/opt/homebrew/bin/rtk"));
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn rtk_installs_the_latest_release_after_its_checksum_matches() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = release(root.path(), ARCHIVE, "rtk", None);
        let runner = tar_runner();
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));

        let path = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap();

        assert_eq!(path, bin_rtk(root.path()));
        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\necho rtk\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let lines = runner.lines();
        assert_eq!(lines.len(), 1, "only the system tar may run: {lines:?}");
        assert!(lines[0].starts_with("tar -xf"), "{lines:?}");
    }

    #[test]
    fn rtk_archive_with_a_folder_level_is_unpacked_the_same_way() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = release(
            root.path(),
            ARCHIVE,
            "rtk-x86_64-unknown-linux-musl/rtk",
            None,
        );
        let ctx = ctx_with(root.path(), &tar_runner(), net(bytes, &checksums));

        let path = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\necho rtk\n");
    }

    #[test]
    fn rtk_checksum_mismatch_stops_before_unpacking() {
        let root = tempfile::tempdir().unwrap();
        let wrong = "0".repeat(64);
        let (bytes, checksums) = release(root.path(), ARCHIVE, "rtk", Some(&wrong));
        let runner = tar_runner();
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));

        let err = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap_err();

        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!bin_rtk(root.path()).exists());
        assert!(
            runner.lines().is_empty(),
            "an unverified archive was unpacked: {:?}",
            runner.lines()
        );
    }

    #[test]
    fn rtk_archive_missing_from_the_checksums_stops_before_unpacking() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = release(root.path(), ARCHIVE, "rtk", None);
        let checksums = format!("{}  other.tar.gz\n", "a".repeat(64));
        let runner = tar_runner();
        let ctx = ctx_with(root.path(), &runner, net(bytes, &checksums));

        let err = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap_err();

        assert!(format!("{err:#}").contains(ARCHIVE), "{err:#}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
        assert!(!bin_rtk(root.path()).exists());
    }

    #[test]
    fn rtk_archive_without_an_rtk_binary_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, checksums) = release(root.path(), ARCHIVE, "README.md", None);
        let ctx = ctx_with(root.path(), &tar_runner(), net(bytes, &checksums));

        let err = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap_err();

        assert!(format!("{err:#}").contains("no rtk binary"), "{err:#}");
        assert!(!bin_rtk(root.path()).exists());
    }

    #[test]
    fn rtk_tag_with_characters_that_change_the_url_downloads_nothing() {
        let root = tempfile::tempdir().unwrap();
        let net = FakeNet::new().with(TAG_URL, r#"{"tag_name":"v9.9.9/../evil"}"#);
        let ctx = ctx_with(root.path(), &tar_runner(), net);

        let err = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap_err();

        assert!(format!("{err:#}").contains("download URL"), "{err:#}");
        assert!(!bin_rtk(root.path()).exists());
    }

    #[test]
    fn rtk_dry_run_downloads_nothing() {
        let root = tempfile::tempdir().unwrap();
        let mut ctx = ctx_with(root.path(), &tar_runner(), FakeNet::new());
        ctx.dry_run = true;

        let path = ensure_rtk_for(&ctx, &RunId::new(), Os::Linux, Arch::X86_64).unwrap();

        assert_eq!(path, bin_rtk(root.path()));
        assert!(!bin_rtk(root.path()).exists());
    }

    #[test]
    fn rtk_asset_names_follow_the_platform() {
        assert_eq!(
            asset_name(Os::MacOs, Arch::Aarch64),
            "rtk-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name(Os::MacOs, Arch::X86_64),
            "rtk-x86_64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name(Os::Linux, Arch::X86_64),
            "rtk-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_name(Os::Linux, Arch::Aarch64),
            "rtk-aarch64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn rtk_defaults_set_pro_hint_false_and_keep_comments() {
        let root = tempfile::tempdir().unwrap();
        let path = rtk_config_file(root.path()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "# keep me\n[display]\n# hint\npro_hint = true\ncolor = true\n",
        )
        .unwrap();
        let ctx = ctx_with(
            root.path(),
            &Recorder::new(FakeRunner::new()),
            FakeNet::new(),
        );

        apply_defaults(&ctx, &ctx.run).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("# hint"), "{text}");
        assert!(text.contains("pro_hint = false"), "{text}");
        assert!(text.contains("color = true"), "{text}");
        assert!(!text.contains("pro_hint = true"), "{text}");
    }

    #[test]
    fn rtk_defaults_create_the_config_when_missing_and_are_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let path = rtk_config_file(root.path()).unwrap();
        let ctx = ctx_with(
            root.path(),
            &Recorder::new(FakeRunner::new()),
            FakeNet::new(),
        );

        apply_defaults(&ctx, &ctx.run).unwrap();
        let once = fs::read_to_string(&path).unwrap();
        assert!(
            once.contains("[display]") && once.contains("pro_hint = false"),
            "{once}"
        );

        apply_defaults(&ctx, &ctx.run).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), once);
    }
}
