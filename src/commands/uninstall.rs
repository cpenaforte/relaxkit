//! `relaxkit uninstall` (spec 6, ruling K106). Removes only what relaxkit wrote: the pack's MCP entries, the config
//! keys the pack wrote, the roles blocks, the sandbox policy, and the pack's binary copies under `dirs.bin`. Every
//! other entry stays, including the user's own agents and the plugins the user installed. Every removal is journaled,
//! so `relaxkit undo` reverts it. The binary itself, the Node link and the pack's plugins are left in place, and the
//! report lists them.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::KitConfig;
use crate::config_apply::claude;
use crate::config_apply::opencode;
use crate::ctx::Ctx;
use crate::edit::{block_remove, jsonc_get, jsonc_remove, jsonc_set};
use crate::host::{Host, HostKind, detect_hosts};
use crate::recipe::{Step, load_all};
use crate::sandbox::{self, opencode as sandbox_opencode};
use crate::state::lockfile::Lockfile;
use crate::state::{RunId, original_text, record_backup, record_link, with_run_lock};

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
                HostKind::Opencode => remove_opencode(ctx, run, host.as_ref(), &mut lines)?,
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
    // F23-3 (K106): a key or list entry that was in the file before relaxkit first wrote it is the user's, and stays.
    let original = original_text(ctx, run, &path)?;
    let original = original.as_deref();
    let rtk_command = format!("\"{}\" hook claude", ctx.dirs.bin.join("rtk").display());
    // F23-8: a scalar key goes only while it still holds the value setup or privacy wrote.
    let mut text = remove_if_written(
        &old,
        original,
        &["env", "SUPERPOWERS_DISABLE_TELEMETRY"],
        &json!("1"),
    )?;
    if !rtk_hook_in(original, &rtk_command)? {
        text = remove_rtk_hook(&text, &rtk_command)?;
    }
    for key in [
        "enabled",
        "autoAllowBashIfSandboxed",
        "failIfUnavailable",
        "enableWeakerNetworkIsolation",
    ] {
        text = remove_if_written(&text, original, &["sandbox", key], &json!(true))?;
    }
    let domains = not_in_original(
        original,
        &["sandbox", "network", "allowedDomains"],
        &sandbox::ALLOWED_DOMAINS,
    )?;
    text = remove_strings(&text, &["sandbox", "network", "allowedDomains"], &domains)?;
    let mut excluded = sandbox::claude::excluded_commands();
    excluded.push("gh *".to_string());
    let excluded: Vec<&str> = excluded.iter().map(String::as_str).collect();
    let excluded = not_in_original(original, &["sandbox", "excludedCommands"], &excluded)?;
    text = remove_strings(&text, &["sandbox", "excludedCommands"], &excluded)?;
    if config.compact_at.is_some() {
        for model in claude::claude_models(&config.for_host(HostKind::Claude)) {
            text = restore_or_remove(
                &text,
                original,
                &["modelSettings", &model, "autoCompactWindow"],
            )?;
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

/// The OpenCode agents and routing block the pack wrote, and the sandbox policy file it wrote.
fn remove_opencode(ctx: &Ctx, run: &RunId, host: &dyn Host, lines: &mut Vec<String>) -> Result<()> {
    opencode::unset_roles(ctx, run, host)?;
    // No compaction buffer is removed: relaxkit writes none on OpenCode (K114), so one present is the user's.
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

/// Removes a scalar key only while it still holds `written`, the value relaxkit set. A value the owner changed by hand
/// stays (F23-8).
fn remove_if_written(
    text: &str,
    original: Option<&str>,
    path: &[&str],
    written: &Value,
) -> Result<String> {
    if jsonc_get(text, path)?.as_ref() == Some(written) {
        restore_or_remove(text, original, path)
    } else {
        Ok(text.to_string())
    }
}

/// Puts back the value the file held before relaxkit wrote the key, or removes the key when the user had none.
fn restore_or_remove(text: &str, original: Option<&str>, path: &[&str]) -> Result<String> {
    match original
        .map(|old| jsonc_get(old, path))
        .transpose()?
        .flatten()
    {
        Some(value) => jsonc_set(text, path, &value),
        None => jsonc_remove(text, path),
    }
}

/// The entries of `wanted` that the list at `path` did not hold before relaxkit wrote it.
fn not_in_original<'a>(
    original: Option<&str>,
    path: &[&str],
    wanted: &[&'a str],
) -> Result<Vec<&'a str>> {
    let mut held = Vec::new();
    if let Some(old) = original
        && let Some(Value::Array(items)) = jsonc_get(old, path)?
    {
        held = items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect();
    }
    Ok(wanted
        .iter()
        .copied()
        .filter(|item| !held.iter().any(|kept| kept == item))
        .collect())
}

/// Whether the file held the relaxkit Claude hook before relaxkit wrote it, so the hook is the user's own.
fn rtk_hook_in(original: Option<&str>, command: &str) -> Result<bool> {
    let Some(old) = original else {
        return Ok(false);
    };
    let Some(Value::Array(entries)) = jsonc_get(old, &["hooks", "PreToolUse"])? else {
        return Ok(false);
    };
    Ok(entries.iter().any(|entry| is_rtk_hook(entry, command)))
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::edit::jsonc_get;
    use crate::exec::FakeRunner;
    use crate::host::HostInfo;
    use crate::host::claude::ClaudeHost;
    use crate::net::FakeNet;
    use crate::platform::Os;
    use crate::sandbox::GhPath;
    use crate::state::safe_write;

    fn get(text: &str, path: &[&str]) -> Option<Value> {
        jsonc_get(text, path).unwrap()
    }

    #[test]
    fn a_user_sandbox_setting_survives_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
        let host = ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/bin/claude"),
                version: "2.1.330".into(),
            },
            root.path().join(".claude"),
            root.path().join(".claude.json"),
        );
        let settings = host.config_file(&ctx);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        // The user's own sandbox settings, on disk before relaxkit first writes the file.
        let user = "{\"env\": {\"USER_KEY\": \"keep\"}, \"sandbox\": {\"enabled\": true, \"failIfUnavailable\": true, \"network\": {\"allowedDomains\": [\"mine.example\"]}}}";
        fs::write(&settings, user).unwrap();
        // setup's sandbox write, through the same text function, journaled like any other change.
        let pack =
            sandbox::claude::settings_text(user, Os::Linux, GhPath::ExcludedFallback).unwrap();
        safe_write(&ctx, &RunId::new(), &settings, &pack).unwrap();

        let mut lines = Vec::new();
        remove_claude(
            &ctx,
            &RunId::new(),
            &host,
            &KitConfig::default(),
            &mut lines,
        )
        .unwrap();

        let text = fs::read_to_string(&settings).unwrap();
        assert_eq!(
            get(&text, &["env", "USER_KEY"]),
            Some(json!("keep")),
            "{text}"
        );
        assert_eq!(
            get(&text, &["sandbox", "enabled"]),
            Some(json!(true)),
            "{text}"
        );
        assert_eq!(
            get(&text, &["sandbox", "failIfUnavailable"]),
            Some(json!(true)),
            "{text}"
        );
        assert_eq!(
            get(&text, &["sandbox", "network", "allowedDomains"]),
            Some(json!(["mine.example"])),
            "the user's domain stays and the pack's are removed: {text}"
        );
        assert_eq!(
            get(&text, &["sandbox", "autoAllowBashIfSandboxed"]),
            None,
            "{text}"
        );
        assert_eq!(get(&text, &["sandbox", "excludedCommands"]), None, "{text}");
        assert_eq!(
            get(&text, &["sandbox", "enableWeakerNetworkIsolation"]),
            None,
            "{text}"
        );
    }

    #[test]
    fn a_hand_changed_sandbox_value_survives_uninstall() {
        // F23-8: a key is removed only while it still holds the value setup wrote, so a value the owner changed stays.
        let root = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
        let host = ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/bin/claude"),
                version: "2.1.330".into(),
            },
            root.path().join(".claude"),
            root.path().join(".claude.json"),
        );
        let settings = host.config_file(&ctx);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let user = "{\"env\": {\"USER_KEY\": \"keep\"}}";
        fs::write(&settings, user).unwrap();
        let pack =
            sandbox::claude::settings_text(user, Os::Linux, GhPath::ExcludedFallback).unwrap();
        let pack = jsonc_set(
            &pack,
            &["env", "SUPERPOWERS_DISABLE_TELEMETRY"],
            &json!("1"),
        )
        .unwrap();
        safe_write(&ctx, &RunId::new(), &settings, &pack).unwrap();
        // The owner changes one setup key by hand after setup.
        let changed = jsonc_set(
            &fs::read_to_string(&settings).unwrap(),
            &["sandbox", "autoAllowBashIfSandboxed"],
            &json!(false),
        )
        .unwrap();
        fs::write(&settings, changed).unwrap();

        let mut lines = Vec::new();
        remove_claude(
            &ctx,
            &RunId::new(),
            &host,
            &KitConfig::default(),
            &mut lines,
        )
        .unwrap();

        let text = fs::read_to_string(&settings).unwrap();
        assert_eq!(
            get(&text, &["sandbox", "autoAllowBashIfSandboxed"]),
            Some(json!(false)),
            "a value the owner changed stays: {text}"
        );
        assert_eq!(get(&text, &["sandbox", "enabled"]), None, "{text}");
        assert_eq!(
            get(&text, &["sandbox", "failIfUnavailable"]),
            None,
            "{text}"
        );
        assert_eq!(
            get(&text, &["env", "SUPERPOWERS_DISABLE_TELEMETRY"]),
            None,
            "{text}"
        );
        assert_eq!(
            get(&text, &["env", "USER_KEY"]),
            Some(json!("keep")),
            "{text}"
        );
    }
}
