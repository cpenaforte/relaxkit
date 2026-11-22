//! caveman's OpenCode source (owner decision, 2026-11-22; spec section 11, exception to O9). The repository is taken at
//! its latest GitHub tag over HTTPS. Its npm package has no plugin files and its releases have no checksum, so the
//! archive is checked entry by entry instead: no absolute names, no `..`, no hard links, and a symbolic link only when
//! it stays inside the archive. Documented limit: no digest or signature check is possible, so a compromised repository
//! or tag would pass.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::net::github_latest_tag;
use crate::runtime::rtk::tag_is_plain;
use crate::runtime::{download_dir, fresh_dir, utf8};
use crate::state::RunId;

/// The repository whose latest tag supplies caveman's OpenCode files.
pub const CAVEMAN_REPO: &str = "JuliusBrussee/caveman";

/// Resolves the latest tag, downloads its repository archive into a fresh per-run directory, checks every entry, and
/// unpacks it there. Returns the repository root (the archive's top folder is stripped). Nothing is unpacked when a
/// check fails.
pub fn fetch_latest_source(ctx: &Ctx, run: &RunId) -> Result<PathBuf> {
    let tag = github_latest_tag(ctx.net.as_ref(), CAVEMAN_REPO)?;
    if !tag_is_plain(&tag) {
        bail!(
            "release tag `{tag}` has characters that do not belong in a download URL; nothing was downloaded"
        );
    }
    let work = download_dir(ctx, run, "caveman");
    fresh_dir(&work)?;
    let archive = work.join("caveman.tar.gz");
    let url = format!("https://codeload.github.com/{CAVEMAN_REPO}/tar.gz/refs/tags/{tag}");
    ctx.net.download(&url, &archive)?;

    check_archive(ctx, &archive)?;

    let extract = work.join("extract");
    fs::create_dir_all(&extract).with_context(|| format!("cannot create {}", extract.display()))?;
    let out = ctx.runner.run(&Cmd::new("tar").args(&[
        "-xf",
        utf8(&archive)?,
        "-C",
        utf8(&extract)?,
        "--strip-components=1",
    ]))?;
    if out.code != 0 {
        bail!(
            "cannot unpack the caveman archive (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(extract)
}

/// Refuses an archive with an unsafe name, a hard link, a link that leaves the archive, or an unknown entry type.
/// The listing is read twice, by name and by type, and the two must line up.
fn check_archive(ctx: &Ctx, archive: &Path) -> Result<()> {
    let names = listing(ctx, archive, "-tf")?;
    let verbose = listing(ctx, archive, "-tvf")?;
    if names.len() != verbose.len() {
        bail!("the caveman archive listing does not match itself; nothing was unpacked");
    }
    for (name, line) in names.iter().zip(verbose.iter()) {
        check_name(name)?;
        if line.contains(&format!("{name} link to ")) || line.starts_with('h') {
            bail!("hard link `{name}` refused; nothing was unpacked");
        }
        match line.chars().next() {
            Some('-' | 'd' | 'g') => {}
            Some('l') => {
                let marker = format!("{name} -> ");
                let target = line
                    .rfind(&marker)
                    .map(|at| &line[at + marker.len()..])
                    .ok_or_else(|| {
                        anyhow!("cannot read the target of symbolic link `{name}`; refused")
                    })?;
                if !link_stays_inside(name, target) {
                    bail!(
                        "symbolic link `{name}` leaves the archive; refused, nothing was unpacked"
                    );
                }
            }
            _ => bail!("archive entry `{name}` has an unsupported type; nothing was unpacked"),
        }
    }
    Ok(())
}

/// Refuses an absolute name or one with a `..` part.
fn check_name(name: &str) -> Result<()> {
    let trimmed = name.trim_end_matches('/');
    if trimmed.starts_with('/') || trimmed.split('/').any(|part| part == "..") {
        bail!(
            "unsafe archive entry `{name}`: absolute names and `..` are refused; nothing was unpacked"
        );
    }
    Ok(())
}

/// The names (`-tf`) or the verbose lines (`-tvf`) of the archive, one per entry.
fn listing(ctx: &Ctx, archive: &Path, flag: &str) -> Result<Vec<String>> {
    let out = ctx
        .runner
        .run(&Cmd::new("tar").args(&[flag, utf8(archive)?]))?;
    if out.code != 0 {
        bail!(
            "cannot list the caveman archive (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(out
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Whether a symbolic link at `entry` with `target` resolves inside the archive's top folder. The top folder is the
/// first part of `entry`, because it is stripped on unpack. An absolute target never stays inside.
pub fn link_stays_inside(entry: &str, target: &str) -> bool {
    if target.starts_with('/') {
        return false;
    }
    // The link's own name is dropped, and so is the stripped top folder.
    let mut depth: Vec<&str> = entry
        .split('/')
        .filter(|part| !part.is_empty())
        .skip(1)
        .collect();
    depth.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            other => depth.push(other),
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::runtime::download_dir;
    use crate::runtime::testing::{Recorder, ctx_with};

    const API: &str = "https://api.github.com/repos/JuliusBrussee/caveman/releases/latest";
    const ARCHIVE: &str =
        "https://codeload.github.com/JuliusBrussee/caveman/tar.gz/refs/tags/v4.1.0";

    /// One archive entry for the test writer.
    struct Entry<'a> {
        name: &'a str,
        kind: u8,
        link: &'a str,
        body: &'a [u8],
    }

    fn file<'a>(name: &'a str, body: &'a [u8]) -> Entry<'a> {
        Entry {
            name,
            kind: b'0',
            link: "",
            body,
        }
    }

    fn dir(name: &str) -> Entry<'_> {
        Entry {
            name,
            kind: b'5',
            link: "",
            body: &[],
        }
    }

    fn link<'a>(name: &'a str, kind: u8, target: &'a str) -> Entry<'a> {
        Entry {
            name,
            kind,
            link: target,
            body: &[],
        }
    }

    /// An uncompressed ustar archive. `tar` reads it the same as a gzipped one.
    fn tar_bytes(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in entries {
            let mut header = [0u8; 512];
            write_field(&mut header[0..100], entry.name.as_bytes());
            write_field(&mut header[100..108], b"0000755");
            write_field(&mut header[108..116], b"0000000");
            write_field(&mut header[116..124], b"0000000");
            write_field(
                &mut header[124..136],
                format!("{:011o}", entry.body.len()).as_bytes(),
            );
            write_field(&mut header[136..148], b"00000000000");
            header[148..156].copy_from_slice(b"        ");
            header[156] = entry.kind;
            write_field(&mut header[157..257], entry.link.as_bytes());
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
            write_field(&mut header[148..156], format!("{sum:06o}").as_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(entry.body);
            let padding = (512 - entry.body.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0u8, padding));
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }

    fn write_field(field: &mut [u8], value: &[u8]) {
        field[..value.len()].copy_from_slice(value);
    }

    /// A network that serves the latest tag and the archive for it.
    fn serve(archive: Vec<u8>) -> FakeNet {
        FakeNet::new()
            .with(API, r#"{"tag_name": "v4.1.0"}"#)
            .with_bytes(ARCHIVE, archive)
    }

    fn runner() -> Recorder {
        Recorder::new(FakeRunner::new()).real("tar")
    }

    #[test]
    fn the_latest_tag_archive_is_unpacked_and_its_root_returned() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[
            dir("caveman-4.1.0/"),
            file(
                "caveman-4.1.0/src/plugins/opencode/plugin.js",
                b"// plugin\n",
            ),
        ]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let source = fetch_latest_source(&ctx, &ctx.run).unwrap();

        assert_eq!(
            fs::read_to_string(source.join("src/plugins/opencode/plugin.js")).unwrap(),
            "// plugin\n"
        );
    }

    #[test]
    fn a_tag_with_path_characters_is_refused_before_any_download() {
        let root = tempfile::tempdir().unwrap();
        let net = FakeNet::new().with(API, r#"{"tag_name": "v1/../x"}"#);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, net);

        let err = format!("{:#}", fetch_latest_source(&ctx, &ctx.run).unwrap_err());

        assert!(err.contains("download URL"), "{err}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn an_entry_that_leaves_the_archive_is_refused_and_nothing_is_unpacked() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[file("caveman-4.1.0/../escape.txt", b"x")]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let err = format!("{:#}", fetch_latest_source(&ctx, &ctx.run).unwrap_err());

        assert!(err.contains("unsafe"), "{err}");
        assert!(!root.path().join("escape.txt").exists());
        assert!(
            !download_dir(&ctx, &ctx.run, "caveman")
                .join("extract")
                .exists(),
            "nothing is unpacked after a refusal"
        );
    }

    #[test]
    fn an_absolute_entry_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[file("/caveman-evil", b"x")]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let err = format!("{:#}", fetch_latest_source(&ctx, &ctx.run).unwrap_err());

        assert!(err.contains("unsafe"), "{err}");
    }

    #[test]
    fn a_symbolic_link_that_leaves_the_archive_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[link("caveman-4.1.0/src/link", b'2', "../../../../outside")]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let err = format!("{:#}", fetch_latest_source(&ctx, &ctx.run).unwrap_err());

        assert!(err.contains("link"), "{err}");
    }

    #[test]
    fn a_symbolic_link_that_stays_inside_the_archive_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[
            file("caveman-4.1.0/src/real.js", b"// real\n"),
            link("caveman-4.1.0/alias.js", b'2', "src/real.js"),
        ]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let source = fetch_latest_source(&ctx, &ctx.run).unwrap();

        assert!(source.join("alias.js").exists());
    }

    #[test]
    fn a_hard_link_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let archive = tar_bytes(&[
            file("caveman-4.1.0/src/real.js", b"// real\n"),
            link("caveman-4.1.0/copy.js", b'1', "caveman-4.1.0/src/real.js"),
        ]);
        let runner = runner();
        let ctx = ctx_with(root.path(), &runner, serve(archive));

        let err = format!("{:#}", fetch_latest_source(&ctx, &ctx.run).unwrap_err());

        assert!(err.contains("hard link"), "{err}");
    }

    #[test]
    fn link_resolution_stays_inside_the_root() {
        assert!(link_stays_inside("caveman-4.1.0/alias.js", "src/real.js"));
        assert!(link_stays_inside("caveman-4.1.0/src/a/link", "../b.js"));
        assert!(!link_stays_inside("caveman-4.1.0/link", "../outside"));
        assert!(!link_stays_inside("caveman-4.1.0/src/link", "../../../x"));
        assert!(!link_stays_inside("caveman-4.1.0/link", "/etc/passwd"));
    }
}
