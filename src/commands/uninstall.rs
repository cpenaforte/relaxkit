//! `relaxkit uninstall` (spec 6, ruling K106). Removes only what relaxkit wrote: the pack's MCP entries, the config
//! keys the pack wrote, the roles blocks, the sandbox policy, and the pack's binary copies under `dirs.bin`. Every
//! other entry stays, including the user's own agents and the plugins the user installed. Every removal is journaled,
//! so `relaxkit undo` reverts it. The binary itself, the Node link and the pack's plugins are left in place, and the
//! report lists them.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config::KitConfig;
use crate::config_apply::claude;
use crate::config_apply::opencode;
use crate::ctx::Ctx;
use crate::edit::{block_remove, jsonc_get, jsonc_remove, jsonc_set};
use crate::host::{Host, HostKind, detect_hosts};
use crate::recipe::{Step, load_all};
use crate::sandbox::{self, opencode as sandbox_opencode};
use crate::state::lockfile::Lockfile;
use crate::state::{RunId, record_backup, record_link, with_run_lock};

/// Removes the pack's entries and returns the report: what was removed, then what is left in place.
pub fn run(ctx: &Ctx) -> Result<Vec<String>> {
    let body = |run: &RunId| -> Result<Vec<String>> {
        let mut lines = Vec::new();
        let config = KitConfig::load(&ctx.dirs)?;
        let names = mcp_names()?;
        for host in detect_hosts(ctx) {
            remove_mcp_entries(ctx, host.as_ref(), &names, &mut lines)?;
            match host.info().kind {
                HostKind::Claude => remove_claude(ctx, run, host.as_ref(), &config, &mut lines)?,
                HostKind::Opencode => {
                    remove_opencode(ctx, run, host.as_ref(), &config, &mut lines)?
                }
            }
        }
        remove_bin_copies(ctx, run, &mut lines)?;
        lines.push("left in place:".to_string());
        leftovers(ctx, &mut lines)?;
        Ok(lines)
    };
    if ctx.dry_run {
        return body(&ctx.run);
    }
    with_run_lock(&ctx.dirs, || body(&ctx.run))
}

/// The MCP server names the recipes register, on any host.
fn mcp_names() -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for recipe in load_all()? {
        for steps in recipe.install.values() {
            for step in steps {
                if let Step::Mcp { name, .. } = step {
                    names.insert(name.clone());
                }
            }
        }
    }
    Ok(names)
}

/// Removes each pack MCP entry the host has. An entry the host does not have is left alone.
fn remove_mcp_entries(
    ctx: &Ctx,
    host: &dyn Host,
    names: &BTreeSet<String>,
    lines: &mut Vec<String>,
) -> Result<()> {
    for name in names {
        if host.mcp_entry_present(ctx, name)? {
            host.mcp_remove(ctx, name)?;
            lines.push(format!(
                "removed MCP entry {name} from {}",
                host.info().kind.product_name()
            ));
        }
    }
    Ok(())
}

/// The Claude Code keys the pack wrote, the roles block in CLAUDE.md, and the compaction threshold when one was set.
fn remove_claude(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    config: &KitConfig,
    lines: &mut Vec<String>,
) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_or_empty(&path)?;
    let rtk_command = format!("\"{}\" hook claude", ctx.dirs.bin.join("rtk").display());
    let mut text = jsonc_remove(&old, &["env", "SUPERPOWERS_DISABLE_TELEMETRY"])?;
    text = remove_rtk_hook(&text, &rtk_command)?;
    for key in [
        "enabled",
        "autoAllowBashIfSandboxed",
        "failIfUnavailable",
        "enableWeakerNetworkIsolation",
    ] {
        text = jsonc_remove(&text, &["sandbox", key])?;
    }
    text = remove_strings(
        &text,
        &["sandbox", "network", "allowedDomains"],
        &sandbox::ALLOWED_DOMAINS,
    )?;
    let mut excluded = sandbox::claude::excluded_commands();
    excluded.push("gh *".to_string());
    let excluded: Vec<&str> = excluded.iter().map(String::as_str).collect();
    text = remove_strings(&text, &["sandbox", "excludedCommands"], &excluded)?;
    if config.compact_at.is_some() {
        for model in claude::claude_models(&config.for_host(HostKind::Claude)) {
            text = jsonc_remove(&text, &["modelSettings", &model, "autoCompactWindow"])?;
        }
    }
    write_changed(ctx, run, &path, &old, &text)?;

    let instructions = host.instructions_file(ctx);
    let old = read_or_empty(&instructions)?;
    let new = block_remove(&old, claude::ROLES_BLOCK);
    write_changed(ctx, run, &instructions, &old, &new)?;
    lines.push("removed the Claude Code keys relaxkit wrote and its roles block".to_string());
    Ok(())
}

/// The OpenCode agents and routing block the pack wrote, its compaction buffer when a threshold was set, and the
/// sandbox policy file it wrote.
fn remove_opencode(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    config: &KitConfig,
    lines: &mut Vec<String>,
) -> Result<()> {
    opencode::unset_roles(ctx, run, host)?;
    if config.compact_at.is_some() {
        let path = host.config_file(ctx);
        let old = read_or_empty(&path)?;
        let new = jsonc_remove(&old, &["compaction", "buffer"])?;
        write_changed(ctx, run, &path, &old, &new)?;
    }
    // Only a regular file is removed: a link is left alone, since undo does not restore links it never journaled.
    let policy = sandbox_opencode::sandbox_config_path(ctx);
    if policy.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
        record_backup(ctx, run, &policy)?;
        if !ctx.dry_run {
            fs::remove_file(&policy)
                .with_context(|| format!("cannot remove {}", policy.display()))?;
        }
        lines.push(format!(
            "removed the OpenCode sandbox policy {}",
            policy.display()
        ));
    }
    lines.push("removed the OpenCode agents and routing block relaxkit wrote".to_string());
    Ok(())
}

/// Removes the rtk hook entry relaxkit wrote, and keeps every other hook.
fn remove_rtk_hook(text: &str, command: &str) -> Result<String> {
    let Some(Value::Array(entries)) = jsonc_get(text, &["hooks", "PreToolUse"])? else {
        return Ok(text.to_string());
    };
    let kept: Vec<Value> = entries
        .iter()
        .filter(|entry| !is_rtk_hook(entry, command))
        .cloned()
        .collect();
    if kept.len() == entries.len() {
        return Ok(text.to_string());
    }
    if kept.is_empty() {
        jsonc_remove(text, &["hooks", "PreToolUse"])
    } else {
        jsonc_set(text, &["hooks", "PreToolUse"], &Value::Array(kept))
    }
}

fn is_rtk_hook(entry: &Value, command: &str) -> bool {
    entry.get("matcher").and_then(Value::as_str) == Some("Bash")
        && entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks
                    .iter()
                    .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
            })
}

/// Removes the listed strings from the array at `path`, and the key when nothing is left.
fn remove_strings(text: &str, path: &[&str], wanted: &[&str]) -> Result<String> {
    let Some(Value::Array(items)) = jsonc_get(text, path)? else {
        return Ok(text.to_string());
    };
    let kept: Vec<Value> = items
        .iter()
        .filter(|item| !item.as_str().is_some_and(|s| wanted.contains(&s)))
        .cloned()
        .collect();
    if kept.len() == items.len() {
        return Ok(text.to_string());
    }
    if kept.is_empty() {
        jsonc_remove(text, path)
    } else {
        jsonc_set(text, path, &Value::Array(kept))
    }
}

/// Removes the pack's binary copies from `dirs.bin`: each regular file (journaled as a backup first) and each link
/// (journaled as a link first), so undo puts them back.
fn remove_bin_copies(ctx: &Ctx, run: &RunId, lines: &mut Vec<String>) -> Result<()> {
    let Ok(entries) = fs::read_dir(&ctx.dirs.bin) else {
        return Ok(());
    };
    for entry in entries {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("cannot inspect {}", path.display()))?;
        if meta.file_type().is_symlink() {
            record_link(ctx, run, &path)?;
        } else if meta.is_file() {
            record_backup(ctx, run, &path)?;
        } else {
            continue;
        }
        if !ctx.dry_run {
            fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
        lines.push(format!("removed the pack copy {}", path.display()));
    }
    Ok(())
}

/// What stays: relaxkit's own binary, its config, lock and journal, the Node link, the pack's plugins, and any
/// `DISABLE_TELEMETRY` the owner opted into.
fn leftovers(ctx: &Ctx, lines: &mut Vec<String>) -> Result<()> {
    if let Ok(exe) = std::env::current_exe() {
        lines.push(format!(
            "  the relaxkit binary at {}: remove it with `brew uninstall relaxkit` if Homebrew owns it, else delete it",
            exe.display()
        ));
    }
    lines.push(format!(
        "  relaxkit's config, lock and journal under {} and {}: kept so `relaxkit undo` still works",
        ctx.dirs.config.display(),
        ctx.dirs.data.display()
    ));
    lines.push(format!(
        "  the Node link {} and the Node 24 it points at (managed by nvm)",
        ctx.dirs.data.join("node").display()
    ));
    if let Some(lock) = Lockfile::load(ctx)? {
        for (name, tool) in &lock.tools {
            lines.push(format!(
                "  plugin or tool {name} {} on {}: remove it with its host's own command",
                tool.version,
                tool.hosts.join(", ")
            ));
        }
    }
    for host in detect_hosts(ctx) {
        if host.info().kind == HostKind::Claude {
            let text = read_or_empty(&host.config_file(ctx))?;
            if jsonc_get(&text, &["env", "DISABLE_TELEMETRY"])?.is_some() {
                lines.push("  env.DISABLE_TELEMETRY in Claude Code's settings (set by privacy --claude-code): remove it by hand if you do not want it".to_string());
            }
        }
    }
    Ok(())
}

fn read_or_empty(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_changed(ctx: &Ctx, run: &RunId, path: &Path, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    crate::state::safe_write(ctx, run, path, new)
}
