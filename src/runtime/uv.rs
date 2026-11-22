//! uv: adopted when it is on the PATH, otherwise a private copy in the pack bin directory.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::platform::is_executable;
use crate::runtime::{download_dir, fresh_dir, utf8};
use crate::state::RunId;

/// The installer that Spike A6 ran with `UV_INSTALL_DIR` and `UV_NO_MODIFY_PATH=1`.
pub const UV_INSTALL_URL: &str = "https://astral.sh/uv/install.sh";

/// The uv to use: the one on the PATH, else the pack copy, else a fresh install into the pack bin dir.
/// The installer never edits a profile, because `UV_NO_MODIFY_PATH=1` is set.
pub fn ensure_uv(ctx: &Ctx, run: &RunId) -> Result<PathBuf> {
    if let Some(path) = ctx.runner.which("uv") {
        return Ok(path);
    }
    let bin = ctx.dirs.bin.join("uv");
    if is_executable(&bin) {
        return Ok(bin);
    }
    if ctx.dry_run {
        ctx.out
            .info(&format!("would install uv into {}", ctx.dirs.bin.display()));
        return Ok(bin);
    }
    let work = download_dir(ctx, run, "uv");
    fresh_dir(&work)?;
    let installer = work.join("install.sh");
    ctx.net.download(UV_INSTALL_URL, &installer)?;
    fs::create_dir_all(&ctx.dirs.bin)
        .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
    let cmd = Cmd::new("sh")
        .arg(utf8(&installer)?)
        .env("UV_INSTALL_DIR", utf8(&ctx.dirs.bin)?)
        .env("UV_NO_MODIFY_PATH", "1");
    let out = ctx.runner.run(&cmd)?;
    if out.code != 0 {
        bail!(
            "the uv installer failed (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    if !is_executable(&bin) {
        bail!("the uv installer did not create {}", bin.display());
    }
    Ok(bin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::runtime::testing::{Recorder, ctx_with};
    use crate::state::journal;

    #[test]
    fn uv_on_path_is_adopted_and_nothing_is_installed() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            FakeRunner::new().with_program("uv", std::path::Path::new("/opt/homebrew/bin/uv")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        let uv = ensure_uv(&ctx, &RunId::new()).unwrap();

        assert_eq!(uv, PathBuf::from("/opt/homebrew/bin/uv"));
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn uv_installs_with_the_flags_from_spike_a6_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let bin_uv = root.path().join(".local/share/relaxkit/bin/uv");
        let runner = Recorder::new(FakeRunner::new().reply("sh ", Output::ok(""))).effect(
            "sh ",
            bin_uv.clone(),
            "#!/bin/sh\n",
        );
        let net = FakeNet::new().with(UV_INSTALL_URL, "echo installer\n");
        let ctx = ctx_with(root.path(), &runner, net);

        let uv = ensure_uv(&ctx, &RunId::new()).unwrap();

        assert_eq!(uv, bin_uv);
        let installs = runner.cmds();
        assert_eq!(installs.len(), 1, "{:?}", runner.lines());
        let env = installs[0].environment();
        assert!(
            env.contains(&(
                "UV_INSTALL_DIR".to_string(),
                root.path()
                    .join(".local/share/relaxkit/bin")
                    .display()
                    .to_string()
            ))
        );
        assert!(env.contains(&("UV_NO_MODIFY_PATH".to_string(), "1".to_string())));
        assert!(
            journal::read(&ctx.dirs).unwrap().is_empty(),
            "the uv install must not edit a profile"
        );
    }
}
