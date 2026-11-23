//! `relaxkit usage ...` (spec 10 item 19, ruling K107). Runs the pinned ccusage through the pack's Node link. The
//! arguments pass through unchanged, and `--offline` is added (the recipe rule: the pricing fetch is the one network
//! path). The version is the locked one, or the recipe's pin when the lock has none.

use anyhow::{Context, Result, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::runtime::nvm::npx_bin;
use crate::state::lockfile::Lockfile;

const TOOL: &str = "ccusage";

/// Runs ccusage with `args` and returns its exit code.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<i32> {
    let version = pinned_version(ctx)?;
    let npx = npx_bin(ctx);
    if !npx.is_file() && !ctx.dry_run {
        bail!("Node is not installed through relaxkit yet; run `relaxkit setup` first");
    }
    let npx = npx
        .to_str()
        .context("the pack's npx path is not valid text")?
        .to_string();
    let mut cmd = Cmd::new(&npx).arg("-y").arg(&format!("{TOOL}@{version}"));
    for arg in args {
        cmd = cmd.arg(arg);
    }
    if !args.iter().any(|arg| arg == "--offline") {
        cmd = cmd.arg("--offline");
    }
    let path = format!(
        "{}:{}",
        ctx.dirs.data.join("node").join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cmd = cmd.env("PATH", &path).inherit_terminal();
    if ctx.dry_run {
        ctx.out.plan(&format!("would run {cmd}"));
        return Ok(0);
    }
    Ok(ctx.runner.run(&cmd)?.code)
}

/// The version the lock names for ccusage. A recipe names no version, so an unlocked ccusage has nothing to run at.
fn pinned_version(ctx: &Ctx) -> Result<String> {
    Lockfile::load(ctx)?
        .and_then(|lock| lock.tools.get(TOOL).cloned())
        .map(|tool| tool.version)
        .with_context(|| format!("{TOOL} is not locked; run `relaxkit setup` first"))
}
