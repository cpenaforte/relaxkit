//! The coding-agent adapters that relaxkit installs into a host (spec section 9). The Claude Code adapter is a plugin
//! that lives in this repository, and the repository's marketplace file lists it. The OpenCode adapter comes later.

use anyhow::Result;

use crate::brand;
use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::host::Host;
use crate::state::RunId;

/// The plugin's name and the marketplace's name. Both come from the files at the repository root and in
/// `adapters/claude-plugin`, so the install spec is `relaxkit@relaxkit`.
const PLUGIN: &str = brand::NAME;
const MARKETPLACE: &str = brand::NAME;

/// Installs the Claude Code adapter into a Claude host: adds the repository as a marketplace, then installs the plugin
/// from it. A plugin that is already listed is left alone, so a second run changes nothing.
pub fn install_claude(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<()> {
    debug_assert_eq!(
        run, &ctx.run,
        "the adapter install must run under the context's run"
    );
    let listed = host
        .plugin_list(ctx)?
        .iter()
        .any(|plugin| plugin.name == PLUGIN && plugin.marketplace.as_deref() == Some(MARKETPLACE));
    if listed {
        return Ok(());
    }
    // A marketplace added by an earlier run whose plugin install then failed is not added twice.
    if !marketplace_listed(ctx) {
        host.marketplace_add(ctx, brand::REPO)?;
    }
    host.plugin_install(ctx, &format!("{PLUGIN}@{MARKETPLACE}"))
}

/// Whether Claude already lists the marketplace. A listing that fails counts as not listed, so the add runs and
/// reports its own error.
fn marketplace_listed(ctx: &Ctx) -> bool {
    let cmd = Cmd::new("claude").args(&["plugin", "marketplace", "list"]);
    match ctx.runner.run(&cmd) {
        Ok(out) if out.code == 0 => out.stdout.lines().any(|line| {
            line.trim()
                .strip_prefix('\u{276f}')
                .and_then(|body| body.split_whitespace().next())
                == Some(MARKETPLACE)
        }),
        _ => false,
    }
}
