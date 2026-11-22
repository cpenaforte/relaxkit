//! OpenCode configuration: one agent per role in `opencode.jsonc`, the routing block in `AGENTS.md`, and the compaction
//! buffer (spec 6.2). Every write goes through `state::safe_write`, and an unchanged file is not written. Only V2 keys
//! are written (`agents`, `compaction`, `plugins`, `mcp.servers`), never the V1 `plugin` key or `mcp.<name>`.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{KitConfig, effective};
use crate::ctx::Ctx;
use crate::edit::{block_remove, block_upsert, jsonc_get, jsonc_remove, jsonc_restore, jsonc_set};
use crate::host::{Host, HostKind};
use crate::roles::{Role, Tier, family_version, resolve_opencode, tier_of};
use crate::state::{OriginalText, RunId, original_text, safe_write};

/// The managed block in AGENTS.md that routes each kind of prompt to its agent.
pub const ROUTING_BLOCK: &str = "roles";

/// The result of `apply_compact`. `KeptDefault` means OpenCode keeps its own compaction setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactNote {
    Applied { buffer: u64 },
    KeptDefault,
}

/// The agents relaxkit manages: the four subagents, then the planner's `plan` agent, which takes only a model.
pub fn managed_agent_ids() -> [&'static str; 5] {
    [
        "implementer",
        "reviewer",
        "implementer-escalated",
        "reviewer-final",
        "plan",
    ]
}

/// The subagents relaxkit defines: the agent id, the role whose setting it runs with, and its description.
const SUBAGENTS: [(&str, Role, &str); 4] = [
    (
        "implementer",
        Role::Implementer,
        "Implements one task of a plan (relaxkit).",
    ),
    (
        "reviewer",
        Role::Reviewer,
        "Reviews one task's changes (relaxkit).",
    ),
    (
        "implementer-escalated",
        Role::Escalation,
        "Implements a task in fix-loop rounds 4 to 5 (relaxkit).",
    ),
    (
        "reviewer-final",
        Role::FinalReview,
        "Reviews the whole branch before merge (relaxkit).",
    ),
];

/// The planner's agent. relaxkit sets only its model.
const PLANNER_AGENT: &str = "plan";

/// Writes the agents for the five roles into `opencode.jsonc`, and the routing block into AGENTS.md. A role whose
/// setting has no OpenCode model in `available` gets no `model` key, so its agent inherits the host default.
pub fn apply_roles(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    config: &KitConfig,
    available: &[String],
) -> Result<()> {
    require_opencode(host)?;
    let path = host.config_file(ctx);
    let old = read_config(&path)?;
    let mut text = old.clone();
    for (id, role, description) in SUBAGENTS {
        text = jsonc_set(&text, &["agents", id, "mode"], &json!("subagent"))?;
        text = jsonc_set(&text, &["agents", id, "description"], &json!(description))?;
        text = set_model(
            &text,
            id,
            resolve_opencode(&effective(config, role), available),
        )?;
    }
    let planner = resolve_opencode(&effective(config, Role::Planner), available);
    text = set_model(&text, PLANNER_AGENT, planner)?;
    write_if_changed(ctx, run, &path, &old, &text)?;

    let agents_path = host.instructions_file(ctx);
    let agents_old = read_optional(&agents_path)?.unwrap_or_default();
    let agents_new = block_upsert(&agents_old, ROUTING_BLOCK, ROUTING_TEXT);
    write_if_changed(ctx, run, &agents_path, &agents_old, &agents_new)
}

/// The routing sentences. Each names one prompt kind and the agent that takes it.
pub const ROUTING_TEXT: &str = "When you dispatch a subagent, pick its agent by the prompt: implementer prompts go to the `implementer` agent; review prompts go to the `reviewer` agent; fix-loop rounds 4 to 5 go to the `implementer-escalated` agent; the final whole-branch review goes to the `reviewer-final` agent. Planning runs in the main session with the `plan` agent. These model choices are set by relaxkit's configuration.";

/// Sets `agents.<id>.model`, or removes it when there is no model, so the agent inherits the host default.
fn set_model(text: &str, id: &str, model: Option<String>) -> Result<String> {
    match model {
        Some(model) => jsonc_set(text, &["agents", id, "model"], &json!(model)),
        None => jsonc_remove(text, &["agents", id, "model"]),
    }
}

/// Sets `compaction.buffer` to the window minus `compact_at`, so OpenCode compacts when `compact_at` tokens are used
/// (spec 6.2). Without a known window, or with a window at or below the target, nothing is written.
pub fn apply_compact(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    compact_at: u64,
    window: Option<u64>,
) -> Result<CompactNote> {
    require_opencode(host)?;
    let Some(window) = window.filter(|window| *window > compact_at) else {
        return Ok(CompactNote::KeptDefault);
    };
    let buffer = window - compact_at;
    let path = host.config_file(ctx);
    let old = read_config(&path)?;
    let text = jsonc_set(&old, &["compaction", "buffer"], &json!(buffer))?;
    write_if_changed(ctx, run, &path, &old, &text)?;
    Ok(CompactNote::Applied { buffer })
}

/// Removes what `apply_roles` wrote, and restores each managed key to the value the file held before relaxkit first
/// wrote it (F23-11). The routing block is removed. A managed agent block is removed only when the original file did
/// not have it and nothing else is left in it. Any other agent the user defined, and the rest of the file, stay as
/// they are. With no earlier journaled write to revert (K117), the config file is left alone.
pub fn unset_roles(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    config: &KitConfig,
    available: &[String],
) -> Result<()> {
    require_opencode(host)?;
    let path = host.config_file(ctx);
    let old = read_config(&path)?;
    let text = match original_text(ctx, run, &path)? {
        OriginalText::NoEarlierRecord => old.clone(),
        OriginalText::Created => strip_managed_agents(&old, None, config, available)?,
        OriginalText::Text(original) => {
            strip_managed_agents(&old, Some(&original), config, available)?
        }
    };
    write_if_changed(ctx, run, &path, &old, &text)?;

    let agents_path = host.instructions_file(ctx);
    let agents_old = read_optional(&agents_path)?.unwrap_or_default();
    let agents_new = block_remove(&agents_old, ROUTING_BLOCK);
    write_if_changed(ctx, run, &agents_path, &agents_old, &agents_new)
}

/// The role an agent runs with: the planner's, or a subagent's.
fn role_of(id: &str) -> Option<Role> {
    if id == PLANNER_AGENT {
        return Some(Role::Planner);
    }
    SUBAGENTS
        .iter()
        .find(|(agent, _, _)| *agent == id)
        .map(|(_, role, _)| *role)
}

/// Whether the managed key should be restored to its original value (K118, K121). An absent key is restored. A key
/// with a known expected value is restored only when it holds that value. When the expected model is unknown (the
/// list is empty or unreadable), a model is restored only when it is an `anthropic/` id of the family relaxkit would
/// have written for the role's tier. Any other value is the owner's and stays.
fn key_restores(
    text: &str,
    config: &KitConfig,
    available: &[String],
    id: &str,
    key: &str,
) -> Result<bool> {
    // K122 follows in F23-19: an absent model is not restored either. Until then only model is restored from absent.
    let Some(current) = jsonc_get(text, &["agents", id, key])? else {
        return Ok(key == "model");
    };
    if let Some(expected) = written_value(config, available, id, key) {
        return Ok(current == expected);
    }
    let Some(role) = role_of(id) else {
        return Ok(false);
    };
    let tier: Tier = tier_of(&effective(config, role));
    let is_family_id = current
        .as_str()
        .and_then(|model| model.strip_prefix("anthropic/"))
        .is_some_and(|model| family_version(model, tier.claude_alias()).is_some());
    Ok(key == "model" && is_family_id)
}

/// The value setup wrote under `agents.<id>.<key>`, or `None` when setup writes no such key (a model no listed id
/// matches gets no `model` key, so the agent inherits the host default).
fn written_value(config: &KitConfig, available: &[String], id: &str, key: &str) -> Option<Value> {
    if id == PLANNER_AGENT {
        return match key {
            "model" => resolve_opencode(&effective(config, Role::Planner), available)
                .map(|model| json!(model)),
            _ => None,
        };
    }
    let (_, role, description) = SUBAGENTS.iter().find(|(agent, _, _)| *agent == id)?;
    match key {
        "mode" => Some(json!("subagent")),
        "description" => Some(json!(description)),
        "model" => resolve_opencode(&effective(config, *role), available).map(|model| json!(model)),
        _ => None,
    }
}

/// The config text with each managed agent key restored to `original`, and the agent blocks that relaxkit alone
/// created removed once they are empty.
fn strip_managed_agents(
    text: &str,
    original: Option<&str>,
    config: &KitConfig,
    available: &[String],
) -> Result<String> {
    let mut text = text.to_string();
    for id in managed_agent_ids() {
        // Only the keys relaxkit wrote come out; a key the user added under the same agent stays (K106). A key
        // comes out only while it holds the value setup wrote (K118).
        let keys: &[&str] = if id == PLANNER_AGENT {
            &["model"]
        } else {
            &["mode", "description", "model"]
        };
        for key in keys {
            if key_restores(&text, config, available, id, key)? {
                text = jsonc_restore(&text, original, &["agents", id, key])?;
            }
        }
        let emptied = jsonc_get(&text, &["agents", id])?
            .and_then(|value| value.as_object().map(|object| object.is_empty()))
            .unwrap_or(false);
        let original_had_block = original
            .map(|old| jsonc_get(old, &["agents", id]))
            .transpose()?
            .flatten()
            .is_some();
        if emptied && !original_had_block {
            text = jsonc_remove(&text, &["agents", id])?;
        }
    }
    Ok(text)
}

fn require_opencode(host: &dyn Host) -> Result<()> {
    if host.info().kind != HostKind::Opencode {
        bail!(
            "OpenCode configuration only applies to OpenCode, not {}",
            host.info().kind
        );
    }
    Ok(())
}

/// The config file's text. A missing or blank file reads as an empty object, so the first write creates it.
fn read_config(path: &Path) -> Result<String> {
    Ok(read_optional(path)?
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "{}\n".to_string()))
}

/// The file's text, or `None` when the file does not exist.
fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
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

    use serde_json::{Value, json};

    use super::*;
    use crate::edit::{block_get, jsonc_get, jsonc_set};
    use crate::exec::FakeRunner;
    use crate::host::opencode::OpencodeHost;
    use crate::host::{HostInfo, HostKind};
    use crate::net::FakeNet;
    use crate::presets::Preset;
    use crate::roles::{ModelSpec, Role, RoleSetting};
    use crate::state::journal;

    /// K33: comments, a trailing comma and a V1 `mcp.legacy` entry.
    const FIXTURE: &str = "{\n  // my settings\n  \"theme\": \"dark\",\n  \"mcp\": {\n    \"legacy\": {\"type\": \"local\", \"command\": [\"old\"]},\n  },\n}\n";

    const MODELS: [&str; 3] = [
        "anthropic/claude-haiku-5-5",
        "anthropic/claude-sonnet-5-5",
        "anthropic/claude-opus-5-5",
    ];

    fn available() -> Vec<String> {
        MODELS.iter().map(|id| id.to_string()).collect()
    }

    fn opencode(root: &Path) -> OpencodeHost {
        OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/bin/opencode"),
                version: "2.2.6".into(),
            },
            root.join(".config/opencode"),
        )
    }

    fn ctx(root: &Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    fn config_path(root: &Path) -> PathBuf {
        root.join(".config/opencode/opencode.jsonc")
    }

    fn agents_path(root: &Path) -> PathBuf {
        root.join(".config/opencode/AGENTS.md")
    }

    fn seed(root: &Path, text: &str) {
        fs::create_dir_all(root.join(".config/opencode")).unwrap();
        fs::write(config_path(root), text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn get(text: &str, path: &[&str]) -> Option<Value> {
        jsonc_get(text, path).unwrap()
    }

    /// The default balanced config with the planner, implementer and reviewer as the preset sets them.
    fn balanced() -> KitConfig {
        KitConfig {
            preset: Preset::Balanced,
            ..KitConfig::default()
        }
    }

    #[test]
    fn managed_agent_ids_are_the_five_pack_names() {
        assert_eq!(
            managed_agent_ids(),
            [
                "implementer",
                "reviewer",
                "implementer-escalated",
                "reviewer-final",
                "plan"
            ]
        );
    }

    #[test]
    fn agents_are_written_for_each_role() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let ctx = ctx(root.path());

        apply_roles(
            &ctx,
            &ctx.run,
            &opencode(root.path()),
            &balanced(),
            &available(),
        )
        .unwrap();

        let text = read(&config_path(root.path()));
        for (id, model) in [
            ("implementer", "anthropic/claude-haiku-5-5"),
            ("reviewer", "anthropic/claude-sonnet-5-5"),
            ("implementer-escalated", "anthropic/claude-sonnet-5-5"),
            ("reviewer-final", "anthropic/claude-opus-5-5"),
        ] {
            assert_eq!(
                get(&text, &["agents", id, "mode"]),
                Some(json!("subagent")),
                "{id}"
            );
            let description = get(&text, &["agents", id, "description"]);
            assert!(
                description.and_then(|v| v.as_str().map(|s| !s.is_empty())) == Some(true),
                "{id} needs a description"
            );
            assert_eq!(
                get(&text, &["agents", id, "model"]),
                Some(json!(model)),
                "{id}"
            );
        }
        assert_eq!(
            get(&text, &["agents", "plan", "model"]),
            Some(json!("anthropic/claude-opus-5-5"))
        );
        assert!(text.contains("// my settings"), "{text}");
        assert!(
            get(&text, &["mcp", "legacy"]).is_some(),
            "the V1 mcp entry must survive: {text}"
        );
    }

    #[test]
    fn unresolved_tier_writes_no_model_key() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let ctx = ctx(root.path());

        apply_roles(&ctx, &ctx.run, &opencode(root.path()), &balanced(), &[]).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "implementer", "model"]),
            None,
            "{text}"
        );
        assert_eq!(get(&text, &["agents", "plan", "model"]), None, "{text}");
        assert_eq!(
            get(&text, &["agents", "implementer", "mode"]),
            Some(json!("subagent"))
        );
    }

    #[test]
    fn a_pin_to_another_provider_is_written_as_is() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let ctx = ctx(root.path());
        let mut config = balanced();
        config.roles.insert(
            Role::Implementer,
            RoleSetting::Pin(ModelSpec::parse("openai/gpt-x#high").unwrap()),
        );

        apply_roles(
            &ctx,
            &ctx.run,
            &opencode(root.path()),
            &config,
            &available(),
        )
        .unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "implementer", "model"]),
            Some(json!("openai/gpt-x#high"))
        );
    }

    #[test]
    fn general_agent_is_not_denied_by_default() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let ctx = ctx(root.path());

        apply_roles(
            &ctx,
            &ctx.run,
            &opencode(root.path()),
            &balanced(),
            &available(),
        )
        .unwrap();

        let text = read(&config_path(root.path()));
        assert!(!text.contains("permission"), "{text}");
        assert_eq!(get(&text, &["agents", "general"]), None, "{text}");
    }

    #[test]
    fn instructions_block_routes_prompts() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        fs::write(agents_path(root.path()), "My own rules.\n").unwrap();
        let ctx = ctx(root.path());

        apply_roles(
            &ctx,
            &ctx.run,
            &opencode(root.path()),
            &balanced(),
            &available(),
        )
        .unwrap();

        let agents = read(&agents_path(root.path()));
        assert!(agents.starts_with("My own rules.\n"), "{agents}");
        let block = block_get(&agents, "roles").expect("the roles block");
        for sentence in [
            "implementer prompts go to the `implementer` agent",
            "review prompts go to the `reviewer` agent",
            "rounds 4 to 5 go to the `implementer-escalated` agent",
            "the final whole-branch review goes to the `reviewer-final` agent",
        ] {
            assert!(block.contains(sentence), "{sentence} missing from: {block}");
        }
    }

    #[test]
    fn a_second_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &ctx.run, &host, &balanced(), &available()).unwrap();
        let after_first = read(&config_path(root.path()));
        let journaled = journal::read(&ctx.dirs).unwrap().len();

        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        assert_eq!(read(&config_path(root.path())), after_first);
        assert_eq!(journal::read(&ctx.dirs).unwrap().len(), journaled);
    }

    #[test]
    fn compaction_buffer_is_window_minus_target() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());

        let note = apply_compact(&ctx, &ctx.run, &host, 256_000, Some(1_000_000)).unwrap();

        assert_eq!(note, CompactNote::Applied { buffer: 744_000 });
        let text = read(&config_path(root.path()));
        assert_eq!(get(&text, &["compaction", "buffer"]), Some(json!(744_000)));

        let before = read(&config_path(root.path()));
        let note = apply_compact(&ctx, &ctx.run, &host, 256_000, Some(256_000)).unwrap();
        assert_eq!(note, CompactNote::KeptDefault);
        let note = apply_compact(&ctx, &ctx.run, &host, 256_000, Some(100_000)).unwrap();
        assert_eq!(note, CompactNote::KeptDefault);
        assert_eq!(
            read(&config_path(root.path())),
            before,
            "a window at or below the target changes nothing"
        );
    }

    #[test]
    fn compaction_is_not_applied_without_a_known_window() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let ctx = ctx(root.path());

        let note = apply_compact(&ctx, &ctx.run, &opencode(root.path()), 256_000, None).unwrap();

        assert_eq!(note, CompactNote::KeptDefault);
        assert_eq!(
            read(&config_path(root.path())),
            FIXTURE,
            "nothing is written"
        );
    }

    #[test]
    fn writes_only_v2_keys() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &ctx.run, &host, &balanced(), &available()).unwrap();
        apply_compact(&ctx, &ctx.run, &host, 256_000, Some(1_000_000)).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(get(&text, &["plugin"]), None, "no V1 plugin key: {text}");
        assert_eq!(
            get(&text, &["mcp"]),
            Some(json!({"legacy": {"type": "local", "command": ["old"]}})),
            "relaxkit writes no mcp entry: {text}"
        );
    }

    #[test]
    fn unset_removes_only_pack_managed_agents() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &ctx.run, &host, &balanced(), &available()).unwrap();
        let with_custom = jsonc_set(
            &read(&config_path(root.path())),
            &["agents", "custom"],
            &json!({"mode": "subagent", "description": "mine"}),
        )
        .unwrap();
        fs::write(config_path(root.path()), &with_custom).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "custom", "description"]),
            Some(json!("mine"))
        );
        assert_eq!(get(&text, &["agents", "implementer"]), None, "{text}");
        assert_eq!(get(&text, &["agents", "plan", "model"]), None, "{text}");
        assert!(text.contains("// my settings"), "{text}");
        assert!(get(&text, &["mcp", "legacy"]).is_some(), "{text}");
    }

    #[test]
    fn unset_keeps_a_user_key_under_a_managed_agent() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &ctx.run, &host, &balanced(), &available()).unwrap();
        let mut with_user = jsonc_set(
            &read(&config_path(root.path())),
            &["agents", "reviewer", "permission"],
            &json!({"bash": "ask"}),
        )
        .unwrap();
        with_user = jsonc_set(
            &with_user,
            &["agents", "plan", "permission"],
            &json!({"edit": "deny"}),
        )
        .unwrap();
        fs::write(config_path(root.path()), &with_user).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "reviewer", "permission", "bash"]),
            Some(json!("ask")),
            "a user key under a managed agent survives: {text}"
        );
        assert_eq!(
            get(&text, &["agents", "plan", "permission", "edit"]),
            Some(json!("deny")),
            "{text}"
        );
        for key in ["mode", "description", "model"] {
            assert_eq!(
                get(&text, &["agents", "reviewer", key]),
                None,
                "{key}: {text}"
            );
        }
        assert_eq!(get(&text, &["agents", "plan", "model"]), None, "{text}");
        assert_eq!(
            get(&text, &["agents", "implementer"]),
            None,
            "an empty managed block is removed: {text}"
        );
    }

    #[test]
    fn a_user_model_before_setup_survives_unset() {
        // F23-11: the managed keys go back to the values the file held before setup, so the user's plan model stays.
        let root = tempfile::tempdir().unwrap();
        let user = jsonc_set(
            FIXTURE,
            &["agents", "plan", "model"],
            &json!("anthropic/claude-sonnet-5-5"),
        )
        .unwrap();
        seed(root.path(), &user);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "plan", "model"]),
            Some(json!("anthropic/claude-sonnet-5-5")),
            "{text}"
        );
        assert_eq!(get(&text, &["agents", "implementer"]), None, "{text}");
    }

    #[test]
    fn a_model_changed_by_hand_after_setup_stays_through_unset() {
        // F23-13 / K118: the user's model before setup is not restored over a value the owner set after setup.
        let root = tempfile::tempdir().unwrap();
        let before = jsonc_set(
            FIXTURE,
            &["agents", "plan", "model"],
            &json!("anthropic/claude-sonnet-5-5"),
        )
        .unwrap();
        seed(root.path(), &before);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();
        let by_hand = jsonc_set(
            &read(&config_path(root.path())),
            &["agents", "plan", "model"],
            &json!("anthropic/claude-haiku-5-5"),
        )
        .unwrap();
        fs::write(config_path(root.path()), &by_hand).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        let text = read(&config_path(root.path()));
        assert_eq!(
            get(&text, &["agents", "plan", "model"]),
            Some(json!("anthropic/claude-haiku-5-5")),
            "{text}"
        );
    }

    fn plan_model(text: &str) -> Option<Value> {
        get(text, &["agents", "plan", "model"])
    }

    #[test]
    fn empty_list_at_uninstall_restores_the_user_model_when_relaxkit_wrote_its_family() {
        // F23-16 / K121 (a): the list is empty at uninstall, relaxkit wrote opus, and the user had sonnet before setup.
        let root = tempfile::tempdir().unwrap();
        let before = jsonc_set(
            FIXTURE,
            &["agents", "plan", "model"],
            &json!("anthropic/claude-sonnet-5-5"),
        )
        .unwrap();
        seed(root.path(), &before);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();
        assert_eq!(
            plan_model(&read(&config_path(root.path()))),
            Some(json!("anthropic/claude-opus-5-5"))
        );

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &[]).unwrap();

        assert_eq!(
            plan_model(&read(&config_path(root.path()))),
            Some(json!("anthropic/claude-sonnet-5-5"))
        );
    }

    #[test]
    fn empty_list_at_uninstall_keeps_a_hand_set_model_from_another_provider() {
        // F23-16 / K121 (b): an id from another provider is the owner's, whatever the list says.
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), FIXTURE);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();
        let by_hand = jsonc_set(
            &read(&config_path(root.path())),
            &["agents", "plan", "model"],
            &json!("openai/gpt-x"),
        )
        .unwrap();
        fs::write(config_path(root.path()), &by_hand).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &[]).unwrap();

        assert_eq!(
            plan_model(&read(&config_path(root.path()))),
            Some(json!("openai/gpt-x"))
        );
    }

    #[test]
    fn an_absent_model_key_is_restored_to_the_original_when_the_list_was_unreadable_at_setup() {
        // F23-16 / K121 (c): setup removed the user's model while the list was empty; uninstall, with a readable list,
        // puts it back.
        let root = tempfile::tempdir().unwrap();
        let before = jsonc_set(
            FIXTURE,
            &["agents", "plan", "model"],
            &json!("anthropic/claude-sonnet-5-5"),
        )
        .unwrap();
        seed(root.path(), &before);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &[]).unwrap();
        assert_eq!(plan_model(&read(&config_path(root.path()))), None);

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        assert_eq!(
            plan_model(&read(&config_path(root.path()))),
            Some(json!("anthropic/claude-sonnet-5-5"))
        );
    }

    #[test]
    fn a_description_the_owner_deleted_after_setup_stays_absent_through_unset() {
        // F23-18 / K122: an absent mode or description is the owner's removal, so it is not restored.
        let root = tempfile::tempdir().unwrap();
        let before = jsonc_set(
            FIXTURE,
            &["agents", "implementer", "description"],
            &json!("my own description"),
        )
        .unwrap();
        seed(root.path(), &before);
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        apply_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();
        let by_hand = jsonc_remove(
            &read(&config_path(root.path())),
            &["agents", "implementer", "description"],
        )
        .unwrap();
        fs::write(config_path(root.path()), &by_hand).unwrap();

        unset_roles(&ctx, &RunId::new(), &host, &balanced(), &available()).unwrap();

        assert_eq!(
            get(
                &read(&config_path(root.path())),
                &["agents", "implementer", "description"]
            ),
            None,
            "the deleted description stays absent"
        );
    }
}
