//! The named code paths a recipe's `handler` steps run. Each one does a step that data alone cannot express.
//! Handlers that belong to later tasks (caveman_opencode, opencode_sandbox_config) are not here,
//! so the engine refuses them by name before any command runs.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::config_apply::claude::{apply_rtk_hook, link_pack_rtk};
use crate::exec::{Cmd, Output};
use crate::host::{Host, HostKind};
use crate::recipe::{Recipe, Step, Vars, VersionSource};
use crate::runtime::chromium;
use crate::runtime::utf8;
use crate::tools::detect::plugin_name;
use crate::tools::engine::{Engine, HandlerFn};

/// Marketplaces that are not built into Claude Code, with the source `claude plugin marketplace add` takes. A
/// marketplace is added only when no marketplace of that name is listed yet.
const KNOWN_MARKETPLACE_SOURCES: [(&str, &str); 1] = [("caveman", "JuliusBrussee/caveman")];

/// The handler registry. A name maps to its code, and anything else is `None`.
pub fn lookup(name: &str) -> Option<HandlerFn> {
    match name {
        "chromium" => Some(install_chromium),
        "caveman_cli" => Some(caveman_cli),
        "serena_init" => Some(serena_init),
        "claude_marketplace" => Some(claude_marketplace),
        "rtk_claude_hook" => Some(rtk_claude_hook),
        _ => None,
    }
}

/// Installs the Chromium build of the exact `@playwright/mcp` version the recipe pins, so the browser always
/// matches the server. It does not resolve a path: only a recipe that names `{chromium}` needs one.
fn install_chromium(
    engine: &Engine<'_>,
    recipe: &Recipe,
    _host: &dyn Host,
    _vars: &mut Vars,
) -> Result<()> {
    let pin = pinned(recipe)?;
    chromium::ensure_chromium(engine.ctx, engine.run, pin)
}

/// Installs the npm CLI at the recipe's pin, through the Node link. The package name comes from the recipe's
/// version source.
fn caveman_cli(
    engine: &Engine<'_>,
    recipe: &Recipe,
    host: &dyn Host,
    _vars: &mut Vars,
) -> Result<()> {
    let pin = pinned(recipe)?;
    let VersionSource::Npm { package } = &recipe.version else {
        bail!(
            "recipe `{}` installs its CLI with npm but has no npm source",
            recipe.id
        );
    };
    let npm = engine.ctx.dirs.data.join("node").join("bin").join("npm");
    let argv = vec![
        utf8(&npm)?.to_string(),
        "install".to_string(),
        "-g".to_string(),
        format!("{package}@{pin}"),
    ];
    let cmd = engine.command(&argv)?;
    engine.run_checked(host, &cmd)
}

/// Runs `serena init`, which is non-interactive and safe to run again: it writes the Serena config file.
fn serena_init(
    engine: &Engine<'_>,
    _recipe: &Recipe,
    host: &dyn Host,
    _vars: &mut Vars,
) -> Result<()> {
    let argv = vec!["serena".to_string(), "init".to_string()];
    let cmd = engine.command(&argv)?;
    engine.run_checked(host, &cmd)
}

/// Resolves the marketplace a Claude plugin comes from, and adds a known marketplace that is missing.
///
/// A plugin spec that names `{marketplace}` is resolved in this order: the marketplace the plugin is already
/// installed from; then the first listed marketplace whose catalog lists the plugin. A catalog is read from disk.
/// If none lists it, the install fails and nothing is installed.
fn claude_marketplace(
    engine: &Engine<'_>,
    recipe: &Recipe,
    host: &dyn Host,
    vars: &mut Vars,
) -> Result<()> {
    if host.info().kind != HostKind::Claude {
        bail!(
            "claude_marketplace only applies to Claude Code, not {}",
            host.info().kind
        );
    }
    let specs: Vec<String> = recipe
        .install
        .get(&HostKind::Claude)
        .into_iter()
        .flatten()
        .filter_map(|step| match step {
            Step::Plugin { spec } => Some(spec.clone()),
            _ => None,
        })
        .collect();
    for spec in specs {
        if spec.contains("{marketplace}") {
            vars.marketplace = resolve_marketplace(engine, host, plugin_name(&spec))?;
        } else if let Some((_, marketplace)) = spec.split_once('@')
            && let Some((_, source)) = KNOWN_MARKETPLACE_SOURCES
                .iter()
                .find(|(name, _)| *name == marketplace)
        {
            let listed = listed_marketplaces(engine, host)?;
            if !listed.iter().any(|name| name == marketplace) {
                host.marketplace_add(engine.ctx, source)?;
            }
        }
    }
    Ok(())
}

fn resolve_marketplace(engine: &Engine<'_>, host: &dyn Host, plugin: &str) -> Result<String> {
    if let Some(installed) = host
        .plugin_list(engine.ctx)?
        .into_iter()
        .find(|entry| entry.name == plugin)
        && let Some(marketplace) = installed.marketplace
    {
        return Ok(marketplace);
    }
    let config_dir = host
        .config_file(engine.ctx)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("the Claude config directory is not known"))?;
    for marketplace in listed_marketplaces(engine, host)? {
        if catalog_lists(&config_dir, &marketplace, plugin)? {
            return Ok(marketplace);
        }
    }
    bail!(
        "no Claude marketplace lists `{plugin}`, so it cannot be installed; nothing was installed"
    )
}

/// Names from `claude plugin marketplace list`. Each listed marketplace starts with the `❯` marker.
fn listed_marketplaces(engine: &Engine<'_>, host: &dyn Host) -> Result<Vec<String>> {
    let cmd = Cmd::new("claude").args(&["plugin", "marketplace", "list"]);
    let out: Output = crate::host::run_read(engine.ctx, &cmd)
        .with_context(|| format!("cannot list marketplaces for {}", host.info().kind))?;
    Ok(parse_marketplace_names(&out.stdout))
}

fn parse_marketplace_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let body = line.trim().strip_prefix('\u{276f}')?.trim();
            body.split_whitespace().next().map(str::to_owned)
        })
        .collect()
}

/// Whether the marketplace's catalog on disk names the plugin. A marketplace with no catalog on disk lists nothing.
fn catalog_lists(config_dir: &Path, marketplace: &str, plugin: &str) -> Result<bool> {
    let path = config_dir
        .join("plugins")
        .join("marketplaces")
        .join(marketplace)
        .join(".claude-plugin")
        .join("marketplace.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("{} is not JSON", path.display()))?;
    Ok(value
        .get("plugins")
        .and_then(Value::as_array)
        .is_some_and(|plugins| {
            plugins
                .iter()
                .any(|entry| entry.get("name").and_then(Value::as_str) == Some(plugin))
        }))
}

/// Writes rtk's `PreToolUse` hook into Claude Code's settings (spike A C6). `rtk init -g` does not write it when it
/// runs without a terminal, so relaxkit does, once, and keeps the other hooks.
fn rtk_claude_hook(
    engine: &Engine<'_>,
    _recipe: &Recipe,
    host: &dyn Host,
    _vars: &mut Vars,
) -> Result<()> {
    if host.info().kind != HostKind::Claude {
        bail!(
            "rtk_claude_hook only applies to Claude Code, not {}",
            host.info().kind
        );
    }
    // The hook names the pack's rtk path, so that path must resolve first. This covers an adopted rtk and an install.
    link_pack_rtk(engine.ctx)?;
    apply_rtk_hook(engine.ctx, engine.run, host)
}

fn pinned(recipe: &Recipe) -> Result<&str> {
    recipe
        .pin
        .as_deref()
        .ok_or_else(|| anyhow!("recipe `{}` has no pinned version", recipe.id))
}
