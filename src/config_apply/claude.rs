//! Claude Code configuration: the compact threshold, the advisory model roles in CLAUDE.md, and the privacy
//! environment. Each function edits one file through `state::safe_write`, and a second run changes nothing.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{KitConfig, effective};
use crate::ctx::Ctx;
use crate::edit::{block_remove, block_upsert, jsonc_append, jsonc_get, jsonc_set};
use crate::host::Host;
use crate::roles::{Role, RoleSetting, resolve_claude};
use crate::state::{RunId, safe_write};

/// The CLAUDE.md block that names the model for each subagent role.
pub const ROLES_BLOCK: &str = "roles";

/// Sets Claude Code's compact threshold (spec 6.2): `autoCompactWindow` under `modelSettings.<model>` for each Claude
/// model the roles resolve to. Missing entries are created. Nothing else in the file changes.
pub fn apply_compact(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    config: &KitConfig,
    tokens: u64,
) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let mut text = old.clone();
    for model in claude_models(config) {
        text = jsonc_set(
            &text,
            &["modelSettings", &model, "autoCompactWindow"],
            &json!(tokens),
        )?;
    }
    write_if_changed(ctx, run, &path, &old, &text)
}

/// The config as Claude Code reads it. A pin to another provider has no Claude alias, so it is dropped and the role
/// keeps the tier it has without that pin (spec 6.1, rule 1). Roles derived from such a role then follow its preset tier.
fn claude_view(config: &KitConfig) -> KitConfig {
    let mut view = config.clone();
    view.roles.retain(
        |_, setting| !matches!(setting, RoleSetting::Pin(spec) if spec.provider != "anthropic"),
    );
    view
}

/// The Claude Code alias a role runs with. Always `Some`: the view holds no pin to another provider.
fn claude_alias(config: &KitConfig, role: Role) -> Option<String> {
    resolve_claude(&effective(&claude_view(config), role))
}

/// The Claude model ids the roles resolve to, in order. A tier writes its family's current model id.
pub fn claude_models(config: &KitConfig) -> BTreeSet<String> {
    Role::ALL
        .into_iter()
        .filter_map(|role| claude_alias(config, role))
        .map(|alias| claude_model_id(&alias))
        .collect()
}

/// The model id behind a Claude Code alias. A family alias maps to its current model; anything else is already an id.
fn claude_model_id(alias: &str) -> String {
    match alias {
        "haiku" => "claude-haiku-5-5".to_string(),
        "sonnet" => "claude-sonnet-5-5".to_string(),
        "opus" => "claude-opus-5-5".to_string(),
        other => other.to_string(),
    }
}

/// Writes the `roles` block into CLAUDE.md: the model each subagent role should use, as Claude Code aliases.
/// A role whose setting names a non-Anthropic model has no alias and is left out. With no alias at all, the
/// block is removed.
pub fn apply_roles(ctx: &Ctx, run: &RunId, host: &dyn Host, config: &KitConfig) -> Result<()> {
    let path = host.instructions_file(ctx);
    let old = read_text(&path)?;
    let text = match roles_block_text(config) {
        Some(body) => block_upsert(&old, ROLES_BLOCK, &body),
        None => block_remove(&old, ROLES_BLOCK),
    };
    write_if_changed(ctx, run, &path, &old, &text)
}

/// The body of the `roles` block that `apply_roles` writes, or `None` when no role has a Claude Code alias.
/// `status` compares the file's block with this text to find hand edits.
pub fn roles_block_text(config: &KitConfig) -> Option<String> {
    let alias = |role: Role| claude_alias(config, role);
    let subagents: Vec<String> = [
        (Role::Implementer, "implementer"),
        (Role::Reviewer, "reviewer"),
        (Role::Escalation, "escalation (fix-loop rounds 4–5)"),
        (Role::FinalReview, "final-review (whole-branch review)"),
    ]
    .into_iter()
    .filter_map(|(role, label)| alias(role).map(|model| format!("{label} → {model}")))
    .collect();
    let mut sentences = Vec::new();
    if !subagents.is_empty() {
        sentences.push(format!(
            "When superpowers dispatches a subagent, set its model: {}.",
            subagents.join("; ")
        ));
    }
    if let Some(model) = alias(Role::Planner) {
        sentences.push(format!(
            "Planning runs in the main session: planner → {model}."
        ));
    }
    if sentences.is_empty() {
        return None;
    }
    sentences.push("These model choices are advisory.".to_string());
    Some(sentences.join(" "))
}

/// Makes the pack's rtk path resolve for the hook. An adopted rtk lives elsewhere on the PATH, so a symbolic link
/// at `dirs.bin/rtk` points at that entry as found on the PATH, not canonicalized, so an upgrade is followed. A copy
/// already there, or a link pointing elsewhere, is left alone and reported. A dry run changes nothing.
pub fn link_pack_rtk(ctx: &Ctx) -> Result<()> {
    let Some(found) = ctx.runner.which("rtk") else {
        return Ok(());
    };
    let link = ctx.dirs.bin.join("rtk");
    match link.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = fs::read_link(&link)
                .with_context(|| format!("cannot read the link {}", link.display()))?;
            if target != found {
                ctx.out.warn(&format!(
                    "{} points at {}, not at the rtk on the PATH ({}); left as it is",
                    link.display(),
                    target.display(),
                    found.display()
                ));
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if ctx.dry_run {
                ctx.out.plan(&format!(
                    "would link {} to {}",
                    link.display(),
                    found.display()
                ));
                return Ok(());
            }
            fs::create_dir_all(&ctx.dirs.bin)
                .with_context(|| format!("cannot create {}", ctx.dirs.bin.display()))?;
            std::os::unix::fs::symlink(&found, &link)
                .with_context(|| format!("cannot link {} to {}", link.display(), found.display()))
        }
        Err(err) => Err(err).with_context(|| format!("cannot inspect {}", link.display())),
    }
}

/// Writes rtk's Claude Code hook (spike A C6): a `PreToolUse` command for the `Bash` tool that runs the pack's rtk
/// copy by its absolute, quoted path (spec 4, item 5: hooks point at a fixed path in relaxkit's directory).
/// `rtk init -g` does not write it non-interactively. Other hooks are kept, and a second run changes nothing.
pub fn apply_rtk_hook(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let command = rtk_hook_command(ctx);
    let entries = match jsonc_get(&old, &["hooks", "PreToolUse"])? {
        None => Vec::new(),
        Some(Value::Array(entries)) => entries,
        Some(_) => bail!("hooks.PreToolUse in {} is not a list", path.display()),
    };
    if entries.iter().any(|entry| is_rtk_hook(entry, &command)) {
        return Ok(());
    }
    // Only the new entry is appended, so the existing hooks keep their text, order and comments.
    let entry = json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": command}]
    });
    let text = jsonc_append(&old, &["hooks", "PreToolUse"], &entry)?;
    write_if_changed(ctx, run, &path, &old, &text)
}

/// The hook command: the pack's rtk copy by absolute path, in double quotes, then `hook claude`.
fn rtk_hook_command(ctx: &Ctx) -> String {
    format!("\"{}\" hook claude", ctx.dirs.bin.join("rtk").display())
}

/// Whether a `PreToolUse` entry is rtk's Bash hook with exactly this command.
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

/// Sets `env.SUPERPOWERS_DISABLE_TELEMETRY` to "1", the one privacy switch relaxkit writes by default.
pub fn apply_privacy_env(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let text = jsonc_set(&old, &["env", "SUPERPOWERS_DISABLE_TELEMETRY"], &json!("1"))?;
    write_if_changed(ctx, run, &path, &old, &text)
}

/// The opt-in that turns off Claude Code's own telemetry: sets `env.DISABLE_TELEMETRY` to "1" and returns the
/// warning the caller shows about what else that switch turns off. Auto-updates belong to a different switch,
/// which this does not set.
pub fn disable_claude_code_telemetry(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<String> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let text = jsonc_set(&old, &["env", "DISABLE_TELEMETRY"], &json!("1"))?;
    write_if_changed(ctx, run, &path, &old, &text)?;
    Ok(CLAUDE_TELEMETRY_WARNING.to_string())
}

/// What the Claude Code telemetry opt-in turns off, in the words the caller shows.
pub const CLAUDE_TELEMETRY_WARNING: &str = "DISABLE_TELEMETRY also stops feature-flag fetching. That removes \
synced claude.ai skills and plugins and /skill-doctor, and may break Remote Control. \
The separate switch CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC also disables auto-updates; relaxkit does not set it.";

/// The file's text, or an empty string when the file does not exist yet. An empty string parses as an empty object.
fn read_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes `new` only when it differs from `old`, so an unchanged file is neither backed up nor journaled.
fn write_if_changed(ctx: &Ctx, run: &RunId, path: &Path, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    safe_write(ctx, run, path, new)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::*;
    use crate::config::KitConfig;
    use crate::edit::block_get;
    use crate::exec::FakeRunner;
    use crate::host::claude::ClaudeHost;
    use crate::host::{HostInfo, HostKind};
    use crate::net::FakeNet;
    use crate::roles::{ModelSpec, Role, RoleSetting};

    fn host(root: &Path) -> ClaudeHost {
        ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/bin/claude"),
                version: "2.1.330".into(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )
    }

    fn ctx(root: &Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    /// The hook command rtk's entry must carry: the pack's rtk copy by absolute path, quoted.
    fn hook_command(root: &Path) -> String {
        format!(
            "\"{}\" hook claude",
            root.join(".local/share/relaxkit/bin/rtk").display()
        )
    }

    fn settings_path(root: &Path) -> PathBuf {
        root.join(".claude/settings.json")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn compact_writes_each_resolved_model_and_no_global_key() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        apply_compact(
            &ctx,
            &ctx.run,
            &host(root.path()),
            &KitConfig::default(),
            256_000,
        )
        .unwrap();

        let value = read_json(&settings_path(root.path()));
        assert!(value.get("autoCompactWindow").is_none(), "{value}");
        for model in ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"] {
            assert_eq!(
                value["modelSettings"][model]["autoCompactWindow"],
                json!(256_000),
                "{model}"
            );
        }
    }

    #[test]
    fn compact_keeps_existing_keys_hooks_and_permissions_in_order() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        fs::write(
            settings_path(root.path()),
            r#"{
  "effortLevel": "high",
  "hooks": {"Stop": []},
  "permissions": {"allow": ["Bash(ls)"]},
  "modelSettings": {"claude-haiku-5-5": {"effort": "low"}}
}
"#,
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply_compact(
            &ctx,
            &ctx.run,
            &host(root.path()),
            &KitConfig::default(),
            256_000,
        )
        .unwrap();

        let value = read_json(&settings_path(root.path()));
        assert_eq!(value["effortLevel"], json!("high"));
        assert_eq!(value["hooks"], json!({"Stop": []}));
        assert_eq!(value["permissions"], json!({"allow": ["Bash(ls)"]}));
        assert_eq!(
            value["modelSettings"]["claude-haiku-5-5"]["effort"],
            json!("low")
        );
        assert_eq!(
            value["modelSettings"]["claude-haiku-5-5"]["autoCompactWindow"],
            json!(256_000)
        );
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["effortLevel", "hooks", "permissions", "modelSettings"]
        );
    }

    #[test]
    fn roles_block_is_written_to_claude_md() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        let claude_md = root.path().join(".claude/CLAUDE.md");
        fs::write(&claude_md, "# My notes\n\nKeep this.\n").unwrap();
        let ctx = ctx(root.path());
        let config = KitConfig::default();

        apply_roles(&ctx, &ctx.run, &host(root.path()), &config).unwrap();

        let text = fs::read_to_string(&claude_md).unwrap();
        assert!(text.starts_with("# My notes\n\nKeep this.\n"), "{text}");
        let body = block_get(&text, "roles").expect("roles block");
        for word in [
            "planner",
            "implementer",
            "reviewer",
            "escalation",
            "final-review",
            "advisory",
        ] {
            assert!(body.contains(word), "{word} missing from {body}");
        }
        assert!(body.contains("implementer → haiku"), "{body}");
        assert!(body.contains("reviewer → sonnet"), "{body}");
        assert!(body.contains("planner → opus"), "{body}");

        let before = text.clone();
        apply_roles(&ctx, &ctx.run, &host(root.path()), &config).unwrap();
        assert_eq!(
            fs::read_to_string(&claude_md).unwrap(),
            before,
            "idempotent"
        );
    }

    #[test]
    fn roles_block_keeps_the_tier_of_a_non_anthropic_planner_pin() {
        // Spec 6.1 rule 1: Claude Code takes only Anthropic models, so the planner keeps its role tier.
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        let mut config = KitConfig::default();
        config.roles.insert(
            Role::Planner,
            RoleSetting::Pin(ModelSpec::parse("openai/gpt-x").unwrap()),
        );

        apply_roles(&ctx, &ctx.run, &host(root.path()), &config).unwrap();

        let text = fs::read_to_string(root.path().join(".claude/CLAUDE.md")).unwrap();
        let body = block_get(&text, "roles").expect("roles block");
        assert!(body.contains("planner → opus"), "{body}");
        assert!(!body.contains("gpt-x"), "{body}");
        assert!(body.contains("implementer → haiku"), "{body}");
    }

    #[test]
    fn escalation_derives_from_a_pinned_implementer_preset_tier() {
        // Spec 6.1 rule 1: the implementer keeps its preset tier on Claude, and escalation is one tier above that.
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        let mut config = KitConfig::default();
        config.roles.insert(
            Role::Implementer,
            RoleSetting::Pin(ModelSpec::parse("openai/gpt-x").unwrap()),
        );

        apply_roles(&ctx, &ctx.run, &host(root.path()), &config).unwrap();

        let text = fs::read_to_string(root.path().join(".claude/CLAUDE.md")).unwrap();
        let body = block_get(&text, "roles").expect("roles block");
        assert!(body.contains("implementer → haiku"), "{body}");
        assert!(
            body.contains("escalation (fix-loop rounds 4–5) → sonnet"),
            "{body}"
        );
    }

    #[test]
    fn privacy_env_sets_only_the_superpowers_switch() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        apply_privacy_env(&ctx, &ctx.run, &host(root.path())).unwrap();

        let text = fs::read_to_string(settings_path(root.path())).unwrap();
        let value = read_json(&settings_path(root.path()));
        let env: Vec<&str> = value["env"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(env, vec!["SUPERPOWERS_DISABLE_TELEMETRY"]);
        assert_eq!(value["env"]["SUPERPOWERS_DISABLE_TELEMETRY"], json!("1"));
        // Keys are matched with their quotes: the superpowers key contains DISABLE_TELEMETRY as a substring.
        for banned in [
            "DO_NOT_TRACK",
            "\"DISABLE_TELEMETRY\"",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            assert!(!text.contains(banned), "{banned} must not be written");
        }
    }

    #[test]
    fn privacy_env_keeps_the_layout_and_escapes() {
        // Spec 5: strict JSON edits keep the file's layout and spelling, not only its values.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        fs::write(
            settings_path(root.path()),
            "{\n    \"effortLevel\": \"caf\\u00e9\"\n}\n",
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply_privacy_env(&ctx, &ctx.run, &host(root.path())).unwrap();

        let text = fs::read_to_string(settings_path(root.path())).unwrap();
        assert!(
            text.contains("\n    \"effortLevel\": \"caf\\u00e9\""),
            "{text}"
        );
    }

    #[test]
    fn rtk_hook_is_written_as_a_bash_pretooluse_command() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        let value = read_json(&settings_path(root.path()));
        assert_eq!(
            value["hooks"]["PreToolUse"],
            json!([{"matcher": "Bash", "hooks": [{"type": "command", "command": hook_command(root.path())}]}])
        );
    }

    #[test]
    fn rtk_hook_runs_the_pack_copy_by_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        let value = read_json(&settings_path(root.path()));
        let command = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let bin = root.path().join(".local/share/relaxkit/bin/rtk");
        assert!(
            command.starts_with(&format!("\"{}\"", bin.display())),
            "{command}"
        );
        assert!(!command.starts_with("rtk"), "{command}");
        assert!(command.ends_with(" hook claude"), "{command}");
    }

    fn write_file(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn pack_rtk(root: &Path) -> PathBuf {
        root.join(".local/share/relaxkit/bin/rtk")
    }

    #[test]
    fn pack_link_points_at_the_path_rtk_when_no_pack_copy_exists() {
        let root = tempfile::tempdir().unwrap();
        let found = root.path().join("homebrew/bin/rtk");
        write_file(&found, "#!/bin/sh\n");
        let ctx = Ctx::for_test(
            root.path(),
            FakeRunner::new().with_program("rtk", &found),
            FakeNet::new(),
        );

        link_pack_rtk(&ctx).unwrap();

        let link = pack_rtk(root.path());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), found, "not canonicalized");
        assert!(fs::metadata(&link).is_ok(), "the link must resolve");
    }

    #[test]
    fn pack_link_is_not_created_on_a_dry_run() {
        let root = tempfile::tempdir().unwrap();
        let found = root.path().join("homebrew/bin/rtk");
        write_file(&found, "#!/bin/sh\n");
        let mut ctx = Ctx::for_test(
            root.path(),
            FakeRunner::new().with_program("rtk", &found),
            FakeNet::new(),
        );
        ctx.dry_run = true;

        link_pack_rtk(&ctx).unwrap();

        assert!(pack_rtk(root.path()).symlink_metadata().is_err());
    }

    #[test]
    fn pack_link_pointing_elsewhere_is_left_and_reported() {
        let root = tempfile::tempdir().unwrap();
        let found = root.path().join("homebrew/bin/rtk");
        let other = root.path().join("other/rtk");
        write_file(&found, "#!/bin/sh\n");
        write_file(&other, "#!/bin/sh\n");
        let link = pack_rtk(root.path());
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        let ctx = Ctx::for_test(
            root.path(),
            FakeRunner::new().with_program("rtk", &found),
            FakeNet::new(),
        );

        link_pack_rtk(&ctx).unwrap();

        assert_eq!(fs::read_link(&link).unwrap(), other);
        assert!(
            ctx.out.lines().iter().any(|line| line.contains("points")),
            "{:?}",
            ctx.out.lines()
        );
    }

    #[test]
    fn pack_link_is_not_made_over_an_existing_copy() {
        let root = tempfile::tempdir().unwrap();
        let found = root.path().join("homebrew/bin/rtk");
        write_file(&found, "#!/bin/sh\n");
        let copy = pack_rtk(root.path());
        write_file(&copy, "copy\n");
        let ctx = Ctx::for_test(
            root.path(),
            FakeRunner::new().with_program("rtk", &found),
            FakeNet::new(),
        );

        link_pack_rtk(&ctx).unwrap();

        assert_eq!(fs::read_to_string(&copy).unwrap(), "copy\n");
        assert!(!copy.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn no_pack_link_without_rtk_on_the_path() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());

        link_pack_rtk(&ctx).unwrap();

        assert!(pack_rtk(root.path()).symlink_metadata().is_err());
    }

    #[test]
    fn rtk_hook_is_written_once() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();
        let once = fs::read_to_string(settings_path(root.path())).unwrap();

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        assert_eq!(
            fs::read_to_string(settings_path(root.path())).unwrap(),
            once
        );
        assert_eq!(once.matches(" hook claude").count(), 1, "{once}");
    }

    #[test]
    fn rtk_hook_keeps_other_hooks_and_settings() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        fs::write(
            settings_path(root.path()),
            r#"{
  "effortLevel": "high",
  "hooks": {"PreToolUse": [{"matcher": "Write", "hooks": [{"type": "command", "command": "lint"}]}]}
}
"#,
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        let value = read_json(&settings_path(root.path()));
        assert_eq!(value["effortLevel"], json!("high"));
        assert_eq!(
            value["hooks"]["PreToolUse"],
            json!([
                {"matcher": "Write", "hooks": [{"type": "command", "command": "lint"}]},
                {"matcher": "Bash", "hooks": [{"type": "command", "command": hook_command(root.path())}]}
            ])
        );
    }

    #[test]
    fn rtk_hook_appends_to_pretooluse_and_keeps_order_and_comments() {
        // Spec 5: the hook is appended; the existing hook objects keep their key order, their text and the comments.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        let before = "{\n  // keep me\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\"matcher\": \"Write\", \"hooks\": [{\"type\": \"command\", \"command\": \"lint\"}]}, // first\n      {\"hooks\": [{\"type\": \"command\", \"command\": \"z-last\"}], \"matcher\": \"Read\"}\n    ]\n  }\n}\n";
        fs::write(settings_path(root.path()), before).unwrap();
        let ctx = ctx(root.path());

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        let text = fs::read_to_string(settings_path(root.path())).unwrap();
        assert!(text.contains("// keep me"), "{text}");
        assert!(text.contains("// first"), "{text}");
        assert!(
            text.contains("{\"hooks\": [{\"type\": \"command\", \"command\": \"z-last\"}], \"matcher\": \"Read\"}"),
            "{text}"
        );
        let first = text.find("\"matcher\": \"Write\"").unwrap();
        let second = text.find("\"matcher\": \"Read\"").unwrap();
        let rtk = text.find(" hook claude").unwrap();
        assert!(first < second && second < rtk, "{text}");
        let entries = jsonc_get(&text, &["hooks", "PreToolUse"]).unwrap().unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 3, "{entries}");

        apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).unwrap();

        assert_eq!(
            fs::read_to_string(settings_path(root.path())).unwrap(),
            text
        );
    }

    #[test]
    fn rtk_hook_refuses_a_pretooluse_value_that_is_not_a_list() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        let original = r#"{"hooks": {"PreToolUse": {}}}"#;
        fs::write(settings_path(root.path()), original).unwrap();
        let ctx = ctx(root.path());

        assert!(apply_rtk_hook(&ctx, &ctx.run, &host(root.path())).is_err());
        assert_eq!(
            fs::read_to_string(settings_path(root.path())).unwrap(),
            original
        );
    }

    #[test]
    fn claude_code_telemetry_opt_in_warns_about_feature_flags() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        let warning = disable_claude_code_telemetry(&ctx, &ctx.run, &host(root.path())).unwrap();

        assert!(warning.contains("feature-flag"), "{warning}");
        assert!(warning.contains("skills and plugins"), "{warning}");
        assert!(warning.contains("auto-update"), "{warning}");
        let value = read_json(&settings_path(root.path()));
        assert_eq!(value["env"]["DISABLE_TELEMETRY"], json!("1"));
    }
}
