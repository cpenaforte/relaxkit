//! rtk: adopted when it is on the PATH, otherwise installed from its release install script at the tag in
//! `RTK_VERSION` (owner decision O8, ruling K17). The installer is downloaded, checked against its SHA-256, and
//! only then run.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::ctx::Ctx;
use crate::edit::toml_set;
use crate::exec::Cmd;
use crate::platform::{is_executable, rtk_config_file};
use crate::runtime::{download_verified, utf8};
use crate::state::RunId;
use crate::state::safe_write;

/// The rtk release whose install script is run.
pub const RTK_VERSION: &str = "v1.2.1";

/// The install script at `RTK_VERSION`.
pub const RTK_INSTALL_URL: &str = "https://raw.githubusercontent.com/rtk-ai/rtk/v1.2.1/install.sh";

/// SHA-256 of that install script. The download must match before anything runs.
pub const RTK_INSTALL_SHA256: &str =
    "85ae798116fa0dda42a627333b36903b71291785c7395ad3634f8419f7adaeb5";

/// The rtk binary to use. The checksum of the binary the script fetches is checked by the script against the
/// release's checksums file; that is the documented limit (spec section 11), not a relaxkit check.
pub fn ensure_rtk(ctx: &Ctx, run: &RunId) -> Result<PathBuf> {
    ensure_rtk_pinned(ctx, run, RTK_INSTALL_URL, RTK_INSTALL_SHA256)
}

fn ensure_rtk_pinned(ctx: &Ctx, run: &RunId, url: &str, expected_sha256: &str) -> Result<PathBuf> {
    if let Some(path) = ctx.runner.which("rtk") {
        return Ok(path);
    }
    let bin = ctx.dirs.bin.join("rtk");
    if is_executable(&bin) {
        return Ok(bin);
    }
    if ctx.dry_run {
        ctx.out.info(&format!(
            "would install rtk {RTK_VERSION} into {}",
            ctx.dirs.bin.display()
        ));
        return Ok(bin);
    }
    let installer = download_verified(ctx, run, "rtk", url, expected_sha256)?;
    fs::create_dir_all(&ctx.dirs.bin)
        .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
    let cmd = Cmd::new("bash")
        .arg(utf8(&installer)?)
        .env("RTK_INSTALL_DIR", utf8(&ctx.dirs.bin)?)
        .env("RTK_VERSION", RTK_VERSION);
    let out = ctx.runner.run(&cmd)?;
    if out.code != 0 {
        bail!(
            "the rtk installer failed (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    if !is_executable(&bin) {
        bail!("the rtk installer did not create {}", bin.display());
    }
    Ok(bin)
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
    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::runtime::sha256_hex;
    use crate::runtime::testing::{Recorder, ctx_with};

    #[test]
    fn rtk_on_the_path_is_adopted() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            FakeRunner::new().with_program("rtk", std::path::Path::new("/opt/homebrew/bin/rtk")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        let path = ensure_rtk(&ctx, &RunId::new()).unwrap();

        assert_eq!(path, PathBuf::from("/opt/homebrew/bin/rtk"));
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn an_install_script_that_does_not_match_its_pin_is_not_run() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(FakeRunner::new().reply("bash ", Output::ok("")));
        let net = FakeNet::new().with(RTK_INSTALL_URL, "tampered\n");
        let ctx = ctx_with(root.path(), &runner, net);
        let pin = sha256_hex(b"echo installer\n");

        let err = ensure_rtk_pinned(&ctx, &RunId::new(), RTK_INSTALL_URL, &pin).unwrap_err();

        assert!(format!("{err:#}").contains("pinned SHA-256"), "{err:#}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn rtk_installs_from_the_verified_script_at_the_pinned_tag() {
        let root = tempfile::tempdir().unwrap();
        let bin_rtk = root.path().join(".local/share/relaxkit/bin/rtk");
        let runner = Recorder::new(FakeRunner::new().reply("bash ", Output::ok(""))).effect(
            "bash ",
            bin_rtk.clone(),
            "#!/bin/sh\n",
        );
        let body = "echo installer\n";
        let net = FakeNet::new().with(RTK_INSTALL_URL, body);
        let ctx = ctx_with(root.path(), &runner, net);
        let pin = sha256_hex(body.as_bytes());

        let path = ensure_rtk_pinned(&ctx, &RunId::new(), RTK_INSTALL_URL, &pin).unwrap();

        assert_eq!(path, bin_rtk);
        let runs = runner.cmds();
        assert_eq!(runs.len(), 1, "{:?}", runner.lines());
        let env = runs[0].environment();
        assert!(
            env.contains(&("RTK_VERSION".to_string(), "v1.2.1".to_string())),
            "{env:?}"
        );
        assert!(
            env.iter().any(|(key, _)| key == "RTK_INSTALL_DIR"),
            "{env:?}"
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
