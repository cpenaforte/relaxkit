//! `relaxkit privacy [--claude-code]` (spec 6.3). Without the flag, the telemetry switches of the pack's own tools
//! are turned off with each tool's own command (rtk, caveman) and the superpowers switch is set in Claude Code's
//! settings. `--claude-code` also turns off Claude Code's own telemetry, after it prints what that switch costs.

use anyhow::{Result, bail};

use crate::config_apply::{claude, privacy};
use crate::ctx::Ctx;
use crate::host::{HostKind, detect_hosts};
use crate::state::{RunId, with_run_lock};
use crate::ui::Mark;

pub fn run(ctx: &Ctx, claude_code: bool) -> Result<()> {
    let body = |run: &RunId| -> Result<()> {
        let hosts = detect_hosts(ctx);
        if claude_code
            && !hosts
                .iter()
                .any(|host| host.info().kind == HostKind::Claude)
        {
            bail!("Claude Code was not found on the PATH, so its telemetry cannot be set");
        }
        privacy::pack_tools_off(ctx, run)?;
        for host in hosts
            .iter()
            .filter(|host| host.info().kind == HostKind::Claude)
        {
            claude::apply_privacy_env(ctx, run, host.as_ref())?;
            if claude_code {
                // The warning is shown before the switch is written, as the spec asks (6.3).
                ctx.out.warn(claude::CLAUDE_TELEMETRY_WARNING);
                claude::disable_claude_code_telemetry(ctx, run, host.as_ref())?;
                done(
                    ctx,
                    "Claude Code telemetry is off (env.DISABLE_TELEMETRY = 1)",
                    "would turn off Claude Code telemetry (env.DISABLE_TELEMETRY = 1)",
                );
            }
        }
        done(
            ctx,
            "rtk and caveman telemetry are off, and superpowers telemetry is disabled in Claude Code",
            "would turn off rtk and caveman telemetry, and disable superpowers telemetry in Claude Code",
        );
        Ok(())
    };
    if ctx.dry_run {
        return body(&ctx.run);
    }
    with_run_lock(&ctx.dirs, || body(&ctx.run))
}

/// A switch that is now off: an outcome, or in a dry run, the plan, since nothing is switched.
fn done(ctx: &Ctx, text: &str, planned: &str) {
    if ctx.dry_run {
        ctx.out.plan(planned);
    } else {
        ctx.out.outcome(Mark::Ok, text);
    }
}
