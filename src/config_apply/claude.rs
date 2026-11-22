//! Claude Code configuration: the compact threshold, the advisory model roles in CLAUDE.md, and the privacy
//! environment. Each function edits one file through `state::safe_write`, and a second run changes nothing.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::{KitConfig, effective};
use crate::ctx::Ctx;
use crate::edit::{block_remove, block_upsert, json_get, json_set};
use crate::host::Host;
use crate::roles::{Role, resolve_claude};
use crate::state::{RunId, safe_write};

/// The CLAUDE.md block that names the model for each subagent role.
const ROLES_BLOCK: &str = "roles";

/// Sets Claude Code's compact threshold: the global `autoCompactWindow`, and the same value under every
/// `modelSettings.<model>` entry that already sets one.
pub fn apply_compact(ctx: &Ctx, run: &RunId, host: &dyn Host, tokens: u64) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let mut text = json_set(&old, &["autoCompactWindow"], &json!(tokens))?;
    let models: Vec<String> = match json_get(&text, &["modelSettings"])? {
        Some(Value::Object(map)) => map
            .iter()
            .filter(|(_, entry)| entry.get("autoCompactWindow").is_some())
            .map(|(model, _)| model.clone())
            .collect(),
        _ => Vec::new(),
    };
    for model in models {
        text = json_set(
            &text,
            &["modelSettings", &model, "autoCompactWindow"],
            &json!(tokens),
        )?;
    }
    write_if_changed(ctx, run, &path, &old, &text)
}

/// Writes the `roles` block into CLAUDE.md: the model each subagent role should use, as Claude Code aliases.
/// A role whose setting names a non-Anthropic model has no alias and is left out. With no alias at all, the
/// block is removed.
pub fn apply_roles(ctx: &Ctx, run: &RunId, host: &dyn Host, config: &KitConfig) -> Result<()> {
    let path = host.instructions_file(ctx);
    let old = read_text(&path)?;
    let alias = |role: Role| resolve_claude(&effective(config, role));
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
    let text = if sentences.is_empty() {
        block_remove(&old, ROLES_BLOCK)
    } else {
        sentences.push("These model choices are advisory.".to_string());
        block_upsert(&old, ROLES_BLOCK, &sentences.join(" "))
    };
    write_if_changed(ctx, run, &path, &old, &text)
}

/// Sets `env.SUPERPOWERS_DISABLE_TELEMETRY` to "1", the one privacy switch relaxkit writes by default.
pub fn apply_privacy_env(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let text = json_set(&old, &["env", "SUPERPOWERS_DISABLE_TELEMETRY"], &json!("1"))?;
    write_if_changed(ctx, run, &path, &old, &text)
}

/// The opt-in that turns off Claude Code's own telemetry: sets `env.DISABLE_TELEMETRY` to "1" and returns the
/// warning the caller shows about what else that switch turns off. Auto-updates belong to a different switch,
/// which this does not set.
pub fn disable_claude_code_telemetry(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<String> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let text = json_set(&old, &["env", "DISABLE_TELEMETRY"], &json!("1"))?;
    write_if_changed(ctx, run, &path, &old, &text)?;
    Ok(CLAUDE_TELEMETRY_WARNING.to_string())
}

/// What the Claude Code telemetry opt-in turns off, in the words the caller shows.
const CLAUDE_TELEMETRY_WARNING: &str = "DISABLE_TELEMETRY also stops feature-flag fetching. That removes \
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

    fn settings_path(root: &Path) -> PathBuf {
        root.join(".claude/settings.json")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn compact_sets_the_global_key_and_every_existing_model_entry() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        fs::write(
            settings_path(root.path()),
            r#"{
  "effortLevel": "high",
  "hooks": {"Stop": []},
  "permissions": {"allow": ["Bash(ls)"]},
  "modelSettings": {"claude-haiku-5-5": {"autoCompactWindow": 400000}}
}
"#,
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply_compact(&ctx, &ctx.run, &host(root.path()), 256_000).unwrap();

        let value = read_json(&settings_path(root.path()));
        assert_eq!(value["autoCompactWindow"], json!(256_000));
        assert_eq!(
            value["modelSettings"]["claude-haiku-5-5"]["autoCompactWindow"],
            json!(256_000)
        );
        assert_eq!(value["effortLevel"], json!("high"));
        assert_eq!(value["hooks"], json!({"Stop": []}));
        assert_eq!(value["permissions"], json!({"allow": ["Bash(ls)"]}));
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            &keys[..4],
            &["effortLevel", "hooks", "permissions", "modelSettings"],
            "key order must survive"
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
    fn roles_block_omits_a_role_that_resolves_to_none() {
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
        assert!(!body.contains("planner"), "{body}");
        assert!(body.contains("implementer → haiku"), "{body}");
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
