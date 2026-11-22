//! The coding-agent adapters that relaxkit installs into a host (spec section 9). The Claude Code adapter is a plugin
//! that lives in this repository, and the repository's marketplace file lists it. The OpenCode adapter is a plugin in
//! `adapters/opencode-plugin`, installed from its pinned release tag.

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

/// The OpenCode plugin's package name, the `name` in `adapters/opencode-plugin/package.json`.
const OPENCODE_PLUGIN: &str = "relaxkit-opencode";
/// The plugin's directory in this repository, which the install spec names after `::path:`.
const OPENCODE_PATH: &str = "adapters/opencode-plugin";

/// The OpenCode install spec for a release tag: the plugin from this repository at that tag, from its subdirectory.
fn opencode_spec(release_tag: &str) -> String {
    format!(
        "{OPENCODE_PLUGIN}@git+https://github.com/{}.git#{release_tag}::path:{OPENCODE_PATH}",
        brand::REPO
    )
}

/// The version an OpenCode host reports for an installed spec: the text after the last `#`, without a leading `v`.
/// This mirrors `to_plugin` in `host/opencode.rs`, which is private, so the adapter compares the value the host reads.
fn listed_version(spec: &str) -> Option<String> {
    spec.rsplit_once('#')
        .map(|(_, tag)| tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Installs the OpenCode adapter into an OpenCode host at `release_tag`, which must be a pinned tag. An entry already
/// at that tag is left alone, so a second run changes nothing. An entry at another tag is replaced by
/// `plugin_update`, which removes the old entry and adds the new one.
pub fn install_opencode(ctx: &Ctx, run: &RunId, host: &dyn Host, release_tag: &str) -> Result<()> {
    debug_assert_eq!(
        run, &ctx.run,
        "the adapter install must run under the context's run"
    );
    let spec = opencode_spec(release_tag);
    let listed = host
        .plugin_list(ctx)?
        .into_iter()
        .find(|plugin| plugin.name == OPENCODE_PLUGIN);
    match listed {
        Some(plugin) if plugin.version == listed_version(&spec) => Ok(()),
        Some(_) => host.plugin_update(ctx, &spec),
        None => host.plugin_install(ctx, &spec),
    }
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
