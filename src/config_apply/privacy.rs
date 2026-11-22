//! The telemetry switches of the pack's own tools. Each tool's own command turns its telemetry off. The generic
//! `DO_NOT_TRACK` is never used: Claude Code also reads it, and it silently turns off Claude Code's feature flags
//! (spec section 6.3).

use std::env;

use anyhow::{Result, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::runtime::utf8;
use crate::state::RunId;

/// The switch each tool documents for turning its telemetry off, as argv.
const SWITCHES: [&[&str]; 2] = [
    &["rtk", "telemetry", "disable"],
    &["caveman", "telemetry", "off"],
];

/// Turns off rtk's and caveman's telemetry with their own commands, and returns the commands it ran, as written.
/// A dry run runs nothing and returns the same list. The commands write the tools' own files; relaxkit does not
/// back those up, as the recipes' own exec steps do not. `_run` keeps the caller's run lock discipline.
pub fn pack_tools_off(ctx: &Ctx, _run: &RunId) -> Result<Vec<String>> {
    let mut flipped = Vec::new();
    for argv in SWITCHES {
        let display = argv.join(" ");
        if ctx.dry_run {
            ctx.out.plan(&format!("would run {display}"));
        } else {
            let cmd = Cmd::new(argv[0])
                .args(&argv[1..])
                .env("PATH", &search_path(ctx)?);
            let out = ctx.runner.run(&cmd)?;
            if out.code != 0 {
                bail!(
                    "`{display}` failed with exit {}: {}",
                    out.code,
                    out.stderr.trim()
                );
            }
        }
        flipped.push(display);
    }
    Ok(flipped)
}

/// The pack's node link, the pack bin and `~/.local/bin`, then the current PATH: the same order the engine uses.
fn search_path(ctx: &Ctx) -> Result<String> {
    let mut dirs = vec![
        utf8(&ctx.dirs.data.join("node").join("bin"))?.to_string(),
        utf8(&ctx.dirs.bin)?.to_string(),
        utf8(&ctx.dirs.home.join(".local").join("bin"))?.to_string(),
    ];
    if let Some(current) = env::var_os("PATH") {
        dirs.push(current.to_string_lossy().into_owned());
    }
    Ok(dirs.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;

    #[test]
    fn pack_tools_off_runs_each_tools_own_switch() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new()
            .reply("rtk telemetry", Output::ok(""))
            .reply("caveman telemetry", Output::ok(""));
        let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());

        let flipped = pack_tools_off(&ctx, &ctx.run).unwrap();

        assert_eq!(
            flipped,
            vec!["rtk telemetry disable", "caveman telemetry off"]
        );
        assert_eq!(fake.calls(), flipped);
        assert!(
            fake.calls()
                .iter()
                .all(|call| !call.contains("DO_NOT_TRACK"))
        );
    }

    #[test]
    fn pack_tools_off_stops_at_a_failed_switch() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("rtk telemetry", Output::fail(1, "boom"));
        let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());

        let err = pack_tools_off(&ctx, &ctx.run).unwrap_err();

        assert!(
            format!("{err:#}").contains("rtk telemetry disable"),
            "{err:#}"
        );
        assert_eq!(fake.calls(), vec!["rtk telemetry disable"]);
    }

    #[test]
    fn pack_tools_off_dry_run_runs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let mut ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());
        ctx.dry_run = true;

        let flipped = pack_tools_off(&ctx, &ctx.run).unwrap();

        assert_eq!(flipped.len(), 2);
        assert!(fake.calls().is_empty());
    }
}
