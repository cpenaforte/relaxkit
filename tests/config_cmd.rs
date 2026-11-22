//! `config`, `preset`, `privacy`, `login`, `undo`, `uninstall` and `usage` (Task 23, rulings K104 to K111). Binary-level
//! tests run against a Scratch home with stub host commands; library-level tests use FakeRunner and temporary roots.

mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use relaxkit::commands::login;
use relaxkit::config::{KitConfig, effective};
use relaxkit::ctx::Ctx;
use relaxkit::edit::{jsonc_get, jsonc_set};
use relaxkit::exec::{FakeRunner, Output};
use relaxkit::net::FakeNet;
use relaxkit::platform::KitDirs;
use relaxkit::presets::Preset;
use relaxkit::roles::{ModelSpec, Role, RoleSetting, Tier};
use relaxkit::state::lockfile::{LockedTool, Lockfile};
use relaxkit::state::{journal, record_backup, safe_write, undo};
use serde_json::{Value, json};
use support::{Scratch, tree};

/// The OpenCode models the stub lists: one `id window` per line.
const MODELS: &str = "anthropic/claude-haiku-5-5 200000\n\
anthropic/claude-sonnet-5-5 200000\n\
anthropic/claude-opus-5-5 400000\n";

fn with_models(scratch: &Scratch, text: &str) {
    fs::write(scratch.state().join("opencode-models"), text).expect("models listing");
}

fn claude_md(home: &Path) -> PathBuf {
    home.join(".claude/CLAUDE.md")
}

fn claude_settings(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// The global OpenCode config. With neither file present, the host writes `opencode.json` (spike B2).
fn opencode_config(home: &Path) -> PathBuf {
    home.join(".config/opencode/opencode.json")
}

fn config_toml(home: &Path) -> PathBuf {
    home.join(".config/relaxkit/config.toml")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The string at `path` in a JSONC file, or `None` when the key is absent.
fn jsonc_str(path: &Path, keys: &[&str]) -> Option<String> {
    let text = read(path);
    jsonc_get(&text, keys)
        .expect("parse the host file")
        .and_then(|value| value.as_str().map(str::to_string))
}

fn opencode_model(home: &Path, agent: &str) -> Option<String> {
    jsonc_str(&opencode_config(home), &["agents", agent, "model"])
}

fn loaded(home: &Path) -> KitConfig {
    KitConfig::load(&KitDirs::rooted(home)).expect("config.toml loads")
}

fn pin(text: &str) -> RoleSetting {
    RoleSetting::Pin(ModelSpec::parse(text).expect("model spec"))
}

// ---- config set and unset --------------------------------------------------------------------------------------

#[test]
fn set_implementer_cheap_writes_both_hosts() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);

    scratch
        .relaxkit()
        .args(["config", "set", "implementer", "cheap"])
        .assert()
        .success();

    let block = read(&claude_md(scratch.home()));
    assert!(block.contains("implementer → haiku"), "{block}");
    assert_eq!(
        opencode_model(scratch.home(), "implementer").as_deref(),
        Some("anthropic/claude-haiku-5-5")
    );
}

#[test]
fn exact_pin_applies_to_opencode_only_for_other_providers() {
    let scratch = Scratch::new();
    with_models(&scratch, &format!("{MODELS}openai/gpt-x 128000\n"));

    scratch
        .relaxkit()
        .args([
            "config",
            "set",
            "reviewer",
            "anthropic/claude-sonnet-5-5#high",
        ])
        .assert()
        .success();
    assert_eq!(
        opencode_model(scratch.home(), "reviewer").as_deref(),
        Some("anthropic/claude-sonnet-5-5#high"),
        "a variant is kept on OpenCode"
    );
    assert!(read(&claude_md(scratch.home())).contains("reviewer → claude-sonnet-5-5"));

    scratch
        .relaxkit()
        .args(["config", "set", "reviewer", "openai/gpt-x"])
        .assert()
        .success();
    assert_eq!(
        opencode_model(scratch.home(), "reviewer").as_deref(),
        Some("openai/gpt-x")
    );
    let shown = scratch
        .relaxkit()
        .args(["config", "show", "reviewer"])
        .output()
        .expect("show runs");
    let shown = String::from_utf8_lossy(&shown.stdout).into_owned();
    assert!(
        shown.contains("applies to OpenCode only"),
        "show must say Claude Code keeps the tier: {shown}"
    );
    assert_eq!(
        loaded(scratch.home()).roles.get(&Role::Reviewer),
        Some(&pin("openai/gpt-x"))
    );
}

#[test]
fn unknown_opencode_model_is_rejected_with_closest_matches() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);

    let assert = scratch
        .relaxkit()
        .args(["config", "set", "implementer", "anthropic/claude-hiku-5-5"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("anthropic/claude-haiku-5-5"), "{stderr}");
    assert!(
        !config_toml(scratch.home()).exists(),
        "a rejected pin writes nothing"
    );
    assert!(!claude_md(scratch.home()).exists());
}

#[test]
fn opencode_pin_without_opencode_warns_and_is_still_stored() {
    // K109 / F23-4: with no OpenCode installed, its pin cannot be checked, so the pin is stored with a warning.
    let scratch = Scratch::claude_only();

    let assert = scratch
        .relaxkit()
        .args(["config", "set", "implementer", "anthropic/claude-hiku-5-5"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains(
            "OpenCode is not installed, so `anthropic/claude-hiku-5-5` is not checked against its model list"
        ),
        "{stderr}"
    );
    assert!(
        read(&config_toml(scratch.home())).contains("claude-hiku-5-5"),
        "the pin is stored"
    );
}

#[test]
fn unknown_opencode_model_is_accepted_with_a_warning_when_the_list_is_unreadable() {
    // No listing file: the stub `opencode models` exits 1, so the list cannot be read.
    let scratch = Scratch::new();

    let assert = scratch
        .relaxkit()
        .args(["config", "set", "implementer", "anthropic/claude-hiku-5-5"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("could not read"), "{stderr}");
    assert_eq!(
        opencode_model(scratch.home(), "implementer").as_deref(),
        Some("anthropic/claude-hiku-5-5")
    );
}

#[test]
fn host_flag_limits_the_change_to_one_host() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);

    scratch
        .relaxkit()
        .args(["config", "set", "reviewer", "smarter", "--host", "opencode"])
        .assert()
        .success();

    assert_eq!(
        opencode_model(scratch.home(), "reviewer").as_deref(),
        Some("anthropic/claude-opus-5-5")
    );
    assert!(
        !claude_md(scratch.home()).exists(),
        "Claude Code was not touched"
    );
    let config = loaded(scratch.home());
    assert_eq!(config.roles.get(&Role::Reviewer), None, "no global pin");
    assert_eq!(
        config
            .for_host(relaxkit::host::HostKind::Opencode)
            .roles
            .get(&Role::Reviewer),
        Some(&RoleSetting::Tier(Tier::Smarter))
    );
}

#[test]
fn unset_returns_the_role_to_the_preset_tier() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args(["preset", "careful"])
        .assert()
        .success();
    scratch
        .relaxkit()
        .args(["config", "set", "implementer", "cheap"])
        .assert()
        .success();
    assert_eq!(
        effective(&loaded(scratch.home()), Role::Implementer),
        RoleSetting::Tier(Tier::Cheap)
    );

    scratch
        .relaxkit()
        .args(["config", "unset", "implementer"])
        .assert()
        .success();

    let config = loaded(scratch.home());
    assert_eq!(config.roles.get(&Role::Implementer), None);
    assert_eq!(
        effective(&config, Role::Implementer),
        RoleSetting::Tier(Tier::Standard),
        "careful's implementer tier"
    );
}

#[test]
fn best_is_rejected_with_a_smarter_hint() {
    let scratch = Scratch::new();

    let assert = scratch
        .relaxkit()
        .args(["config", "set", "planner", "best"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("smarter"), "{stderr}");
    assert!(!config_toml(scratch.home()).exists());
}

#[test]
fn compact_at_256k_writes_both_hosts() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    // A buffer the user set is not relaxkit's: compact-at must leave it as it is (K114).
    let path = opencode_config(scratch.home());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        jsonc_set("{}", &["compaction", "buffer"], &json!(40_000)).unwrap(),
    )
    .unwrap();

    let assert = scratch
        .relaxkit()
        .args(["config", "set", "compact-at", "256k"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    let settings: Value = serde_json::from_str(&read(&claude_settings(scratch.home())))
        .expect("settings.json is JSON");
    assert_eq!(
        settings["modelSettings"]["claude-opus-5-5"]["autoCompactWindow"],
        json!(256_000),
        "Claude Code's threshold is absolute, so it is written"
    );
    // K114: OpenCode has no verified window source yet, so its compaction buffer is not written.
    assert_eq!(
        jsonc_get(
            &read(&opencode_config(scratch.home())),
            &["compaction", "buffer"]
        )
        .unwrap(),
        Some(json!(40_000)),
        "no OpenCode buffer is written without a verified window source"
    );
    assert!(
        stderr.contains("no verified window source"),
        "the warning must say why OpenCode was left alone: {stderr}"
    );
}

// ---- show ------------------------------------------------------------------------------------------------------

#[test]
fn show_json_lists_both_hosts() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);

    let output = scratch
        .relaxkit()
        .args(["--json", "config", "show"])
        .output()
        .expect("show runs");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let document: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let roles = document["roles"].as_array().expect("a roles list");
    assert_eq!(roles.len(), 5, "{stdout}");
    for role in roles {
        assert!(role.get("claude-code").is_some(), "{role}");
        assert!(role.get("opencode").is_some(), "{role}");
    }
    assert_eq!(roles[0]["role"], json!("planner"));
    assert_eq!(roles[0]["setting"], json!("smarter"));
}

// ---- preset and privacy ----------------------------------------------------------------------------------------

#[test]
fn preset_changes_tiers_but_keeps_pins() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args([
            "config",
            "set",
            "reviewer",
            "anthropic/claude-sonnet-5-5#high",
        ])
        .assert()
        .success();
    scratch
        .relaxkit()
        .args(["config", "set", "implementer", "cheap"])
        .assert()
        .success();

    scratch
        .relaxkit()
        .args(["preset", "careful"])
        .assert()
        .success();

    let config = loaded(scratch.home());
    assert_eq!(config.preset, Preset::Careful);
    assert_eq!(
        config.roles.get(&Role::Reviewer),
        Some(&pin("anthropic/claude-sonnet-5-5#high")),
        "the pin stays"
    );
    assert_eq!(
        effective(&config, Role::Implementer),
        RoleSetting::Tier(Tier::Standard),
        "the tier follows the preset"
    );
    assert_eq!(
        effective(&config, Role::Planner),
        RoleSetting::Tier(Tier::Smarter)
    );
}

#[test]
fn privacy_claude_code_prints_the_warning_then_applies() {
    let scratch = Scratch::new();

    let assert = scratch
        .relaxkit()
        .args(["privacy", "--claude-code"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("feature-flag"),
        "the warning comes first: {stderr}"
    );
    assert_eq!(
        jsonc_str(
            &claude_settings(scratch.home()),
            &["env", "DISABLE_TELEMETRY"]
        )
        .as_deref(),
        Some("1")
    );

    scratch.clear_calls();
    scratch.relaxkit().arg("privacy").assert().success();
    let calls = scratch.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("rtk telemetry disable")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("caveman telemetry off")),
        "{calls:?}"
    );
}

#[test]
fn dry_run_changes_nothing_for_config_privacy_and_preset() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    let before = tree(scratch.home());

    scratch
        .relaxkit()
        .args(["--dry-run", "config", "set", "implementer", "cheap"])
        .assert()
        .success();
    scratch
        .relaxkit()
        .args(["--dry-run", "privacy", "--claude-code"])
        .assert()
        .success();
    scratch
        .relaxkit()
        .args(["--dry-run", "preset", "careful"])
        .assert()
        .success();

    assert_eq!(tree(scratch.home()), before, "a dry run writes nothing");
}

// ---- project scope ---------------------------------------------------------------------------------------------

#[test]
fn project_flag_writes_project_level_files() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    let project = scratch.home().join("work/app");
    fs::create_dir_all(&project).expect("project dir");

    scratch
        .relaxkit()
        .current_dir(&project)
        .args(["--project", "config", "set", "implementer", "cheap"])
        .assert()
        .success();
    assert!(read(&project.join("CLAUDE.md")).contains("implementer → haiku"));
    assert_eq!(
        jsonc_str(
            &project.join(".opencode/opencode.jsonc"),
            &["agents", "implementer", "model"]
        )
        .as_deref(),
        Some("anthropic/claude-haiku-5-5")
    );
    assert!(read(&project.join("AGENTS.md")).contains("implementer"));

    scratch
        .relaxkit()
        .current_dir(&project)
        .args(["--project", "config", "set", "voice", "ultracave"])
        .assert()
        .success();
    let caveman: Value = serde_json::from_str(&read(&project.join(".caveman.json"))).unwrap();
    assert_eq!(caveman["defaultMode"], json!("ultracave"));

    scratch
        .relaxkit()
        .current_dir(&project)
        .args(["--project", "config", "set", "compact-at", "256k"])
        .assert()
        .success();
    let settings: Value =
        serde_json::from_str(&read(&project.join(".claude/settings.json"))).unwrap();
    assert_eq!(
        settings["modelSettings"]["claude-haiku-5-5"]["autoCompactWindow"],
        json!(256_000)
    );

    assert!(
        !claude_md(scratch.home()).exists(),
        "the global files stay untouched"
    );
}

// ---- login -----------------------------------------------------------------------------------------------------

const GH: &str = "/usr/local/bin/gh";

#[test]
fn login_github_runs_the_flow_and_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let logged_out = FakeRunner::new()
        .with_program("gh", Path::new(GH))
        .reply(
            &format!("{GH} auth status"),
            Output::fail(1, "not logged in"),
        )
        .reply(&format!("{GH} auth login"), Output::ok(""))
        .reply(&format!("{GH} auth setup-git"), Output::ok(""));
    let ctx = Ctx::for_test(root.path(), logged_out.clone(), FakeNet::new());

    let code = login::run_with(&ctx, true).expect("login runs");

    assert_eq!(code, 0);
    let calls = logged_out.calls();
    assert!(
        calls.contains(&format!("{GH} auth login --web --clipboard")),
        "{calls:?}"
    );
    assert!(calls.contains(&format!("{GH} auth setup-git")), "{calls:?}");

    let logged_in = FakeRunner::new()
        .with_program("gh", Path::new(GH))
        .reply(&format!("{GH} auth status"), Output::ok("Logged in"));
    let ctx = Ctx::for_test(root.path(), logged_in.clone(), FakeNet::new());

    login::run_with(&ctx, true).expect("login runs");

    let calls = logged_in.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("auth login") && !call.contains("setup-git")),
        "no login call runs when gh is already logged in: {calls:?}"
    );
}

// ---- undo ------------------------------------------------------------------------------------------------------

#[test]
fn undo_restores_the_last_run() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args(["config", "set", "implementer", "cheap"])
        .assert()
        .success();
    assert!(config_toml(scratch.home()).exists());
    assert!(claude_md(scratch.home()).exists());

    scratch.relaxkit().arg("undo").assert().success();

    assert!(
        !config_toml(scratch.home()).exists(),
        "the file the run created is removed"
    );
    assert!(!claude_md(scratch.home()).exists());
    assert!(!opencode_config(scratch.home()).exists());
}

#[test]
fn undo_of_an_older_group_is_refused_when_a_later_group_changed_the_same_file() {
    // K117 / F23-6: undoing an older group would leave the later group's change to the same file without its base.
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch.relaxkit().arg("privacy").assert().success();
    let journal_text = read(&scratch.home().join(".local/share/relaxkit/journal.jsonl"));
    let first: Value = serde_json::from_str(journal_text.lines().next().unwrap()).unwrap();
    let older = first
        .get("group")
        .or_else(|| first.get("run"))
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    scratch
        .relaxkit()
        .args(["config", "set", "compact-at", "256k"])
        .assert()
        .success();
    let before = tree(scratch.home());

    let assert = scratch
        .relaxkit()
        .args(["undo", "--run", &older])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("is not the newest change to")
            && stderr.contains("undo the later group first"),
        "{stderr}"
    );
    assert_eq!(
        tree(scratch.home()),
        before,
        "a refused undo writes nothing"
    );

    // Undo of the newest group still works.
    scratch.relaxkit().arg("undo").assert().success();
    assert!(!read(&claude_settings(scratch.home())).contains("autoCompactWindow"));
}

#[test]
fn undo_reverts_a_run_group_as_one() {
    let root = tempfile::tempdir().unwrap();
    let dirs = KitDirs::rooted(root.path());
    let parent = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let group = parent.run.clone();
    let x = root.path().join("x.txt");
    let y = root.path().join("y.txt");
    let z = root.path().join("z.txt");
    fs::write(&x, "x0").unwrap();
    fs::write(&y, "y0").unwrap();
    fs::write(&z, "z0").unwrap();

    // The parent is the self step, the child is the tool run it re-executes (K105).
    safe_write(&parent, &parent.run, &x, "x1").unwrap();
    let mut child = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    child.group = Some(group.clone());
    safe_write(&child, &child.run, &y, "y1").unwrap();
    assert!(
        journal::read(&dirs)
            .unwrap()
            .iter()
            .all(|record| record.group.as_ref() == Some(&group)),
        "every record carries its group"
    );

    // A newer, separate group.
    let later = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    safe_write(&later, &later.run, &z, "z1").unwrap();

    undo(&parent, Some(&group)).unwrap();

    assert_eq!(
        fs::read_to_string(&x).unwrap(),
        "x0",
        "the self step is reverted"
    );
    assert_eq!(
        fs::read_to_string(&y).unwrap(),
        "y0",
        "the child run is reverted with it"
    );
    assert_eq!(
        fs::read_to_string(&z).unwrap(),
        "z1",
        "the newer group stays"
    );

    undo(&parent, None).unwrap();
    assert_eq!(
        fs::read_to_string(&z).unwrap(),
        "z0",
        "no group means the newest one"
    );
}

#[test]
fn undo_puts_the_binary_back_through_the_canonical_path_only() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real/relaxkit");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, "old binary").unwrap();
    let link = root.path().join("bin/relaxkit");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&real, &link).unwrap();
    let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let run = ctx.run.clone();

    // The swap journals the canonical file, then writes it.
    record_backup(&ctx, &run, &link).unwrap();
    fs::write(&real, "new binary").unwrap();

    undo(&ctx, Some(&run)).unwrap();

    assert_eq!(fs::read_to_string(&real).unwrap(), "old binary");
    assert!(link.is_symlink(), "the link itself is not replaced");
    assert_eq!(fs::read_link(&link).unwrap(), real);
}

#[test]
fn undo_restores_the_lock_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let dirs = KitDirs::rooted(root.path());
    let first = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    lock_with("20.0.51").save(&first, &first.run).unwrap();

    let second = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let group = second.run.clone();
    let before = Lockfile::load(&second).unwrap().unwrap();
    before.snapshot(&second, &second.run).unwrap();
    lock_with("20.0.52").save(&second, &second.run).unwrap();
    assert_eq!(
        Lockfile::load(&second).unwrap().unwrap(),
        lock_with("20.0.52")
    );

    undo(&second, Some(&group)).unwrap();

    assert_eq!(
        Lockfile::load(&second).unwrap().unwrap(),
        lock_with("20.0.51"),
        "{}",
        dirs.config.join("lock.toml").display()
    );
}

fn lock_with(version: &str) -> Lockfile {
    Lockfile {
        relaxkit: "0.1.0".into(),
        tools: [(
            "ccusage".to_string(),
            LockedTool {
                version: version.into(),
                route: "npm".into(),
                hosts: vec!["claude".into()],
                host_versions: Default::default(),
            },
        )]
        .into_iter()
        .collect(),
    }
}

#[test]
fn privacy_refuses_project_and_writes_nothing() {
    // K115 / F23-9: the rtk and caveman switches are tool-wide, so privacy is global like preset and uninstall.
    let scratch = Scratch::new();
    let before = tree(scratch.home());

    let assert = scratch
        .relaxkit()
        .args(["--project", "privacy", "--claude-code"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("--project does not apply to privacy; it is global"),
        "{stderr}"
    );
    assert_eq!(
        tree(scratch.home()),
        before,
        "a refused --project writes nothing"
    );
}

#[test]
fn preset_and_uninstall_refuse_project_and_write_nothing() {
    // K113 / F23-5: both are global by spec, so --project is refused rather than ignored.
    let scratch = Scratch::new();
    let before = tree(scratch.home());

    for (command, args) in [
        ("preset", vec!["preset", "balanced"]),
        ("uninstall", vec!["uninstall"]),
    ] {
        let assert = scratch
            .relaxkit()
            .arg("--project")
            .args(&args)
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
        assert!(
            stderr.contains(&format!(
                "--project does not apply to {command}; it is global"
            )),
            "{stderr}"
        );
    }
    assert_eq!(
        tree(scratch.home()),
        before,
        "a refused --project writes nothing"
    );
}

#[test]
fn uninstall_keeps_a_user_compaction_buffer() {
    // K114 / F23-10: relaxkit writes no compaction buffer on OpenCode, so a buffer present is the user's.
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args(["config", "set", "compact-at", "256k"])
        .assert()
        .success();
    let path = opencode_config(scratch.home());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let config = jsonc_set("{}", &["compaction", "buffer"], &json!(40_000)).unwrap();
    fs::write(&path, config).unwrap();

    scratch.relaxkit().arg("uninstall").assert().success();

    assert_eq!(
        jsonc_get(
            &read(&opencode_config(scratch.home())),
            &["compaction", "buffer"]
        )
        .unwrap(),
        Some(json!(40_000)),
        "the user's buffer stays"
    );
}

#[test]
fn uninstall_keeps_a_compact_window_the_owner_changed_by_hand() {
    // K118 / F23-12: autoCompactWindow goes only while it still holds the value compact-at wrote.
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args(["config", "set", "compact-at", "256k"])
        .assert()
        .success();
    let path = claude_settings(scratch.home());
    let changed = jsonc_set(
        &read(&path),
        &["modelSettings", "claude-opus-5-5", "autoCompactWindow"],
        &json!(200_000),
    )
    .unwrap();
    fs::write(&path, changed).unwrap();

    scratch.relaxkit().arg("uninstall").assert().success();

    let settings: Value = serde_json::from_str(&read(&path)).unwrap();
    assert_eq!(
        settings["modelSettings"]["claude-opus-5-5"]["autoCompactWindow"],
        json!(200_000),
        "a value the owner set stays"
    );
}

// ---- uninstall -------------------------------------------------------------------------------------------------

#[test]
fn uninstall_removes_only_pack_managed_entries() {
    let scratch = Scratch::new();
    with_models(&scratch, MODELS);
    scratch
        .relaxkit()
        .args(["config", "set", "implementer", "cheap"])
        .assert()
        .success();
    let home = scratch.home().to_path_buf();
    let bin = home.join(".local/share/relaxkit/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("rtk"), "rtk copy").unwrap();

    // The user's own settings are on disk before relaxkit first writes the file. `privacy` then journals that
    // file (F23-7), so the pack's entries written after it have an earlier record to revert to.
    fs::write(
        claude_settings(&home),
        "{\"env\": {\"USER_KEY\": \"keep\"}, \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Write\", \"hooks\": [{\"type\": \"command\", \"command\": \"user-hook\"}]}]}}\n",
    )
    .unwrap();
    scratch.relaxkit().arg("privacy").assert().success();

    // Pack-written Claude entries next to user ones.
    let rtk_command = format!("\"{}\" hook claude", bin.join("rtk").display());
    let settings = json!({
        "env": {"USER_KEY": "keep", "SUPERPOWERS_DISABLE_TELEMETRY": "1"},
        "hooks": {"PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": rtk_command}]},
            {"matcher": "Write", "hooks": [{"type": "command", "command": "user-hook"}]}
        ]}
    });
    fs::write(claude_settings(&home), format!("{settings:#}\n")).unwrap();
    let claude_text = format!("# mine\n\n{}", read(&claude_md(&home)));
    fs::write(claude_md(&home), claude_text).unwrap();

    // User-owned OpenCode entries next to the pack's agents and MCP server.
    let mut config = read(&opencode_config(&home));
    config = jsonc_set(&config, &["agents", "custom"], &json!({"mode": "subagent"})).unwrap();
    config = jsonc_set(
        &config,
        &["mcp", "servers", "serena"],
        &json!({"type": "local", "command": ["serena"]}),
    )
    .unwrap();
    config = jsonc_set(
        &config,
        &["mcp", "servers", "mine"],
        &json!({"type": "local", "command": ["mine"]}),
    )
    .unwrap();
    fs::write(opencode_config(&home), config).unwrap();
    fs::write(
        home.join(".claude.json"),
        "{\"mcpServers\": {\"serena\": {}, \"mine\": {}}}\n",
    )
    .unwrap();

    let output = scratch
        .relaxkit()
        .arg("uninstall")
        .output()
        .expect("uninstall runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    let settings: Value = serde_json::from_str(&read(&claude_settings(&home))).unwrap();
    assert_eq!(settings["env"]["USER_KEY"], json!("keep"));
    assert!(
        settings["env"]
            .get("SUPERPOWERS_DISABLE_TELEMETRY")
            .is_none()
    );
    let hooks = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(hooks.len(), 1, "{settings}");
    assert_eq!(hooks[0]["hooks"][0]["command"], json!("user-hook"));

    let claude = read(&claude_md(&home));
    assert!(claude.contains("# mine"), "{claude}");
    assert!(!claude.contains("haiku"), "{claude}");

    assert!(opencode_model(&home, "implementer").is_none());
    assert_eq!(
        jsonc_str(&opencode_config(&home), &["agents", "custom", "mode"]).as_deref(),
        Some("subagent"),
        "agents.custom is user-owned"
    );
    assert!(
        jsonc_get(
            &read(&opencode_config(&home)),
            &["mcp", "servers", "serena"]
        )
        .unwrap()
        .is_none()
    );
    assert!(
        jsonc_get(&read(&opencode_config(&home)), &["mcp", "servers", "mine"])
            .unwrap()
            .is_some()
    );

    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("mcp remove --scope user serena")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|c| !c.contains("remove --scope user mine")),
        "{calls:?}"
    );
    assert!(
        !bin.join("rtk").exists(),
        "the pack's binary copy is removed"
    );
    assert!(stdout.contains("left in place"), "{stdout}");
}

// ---- usage -----------------------------------------------------------------------------------------------------

/// Makes the pack's Node link point at the scratch Node 24, as setup does.
fn link_node(scratch: &Scratch) {
    let data = scratch.home().join(".local/share/relaxkit");
    fs::create_dir_all(&data).unwrap();
    symlink(
        scratch.home().join(".nvm/versions/node/v24.13.0"),
        data.join("node"),
    )
    .unwrap();
}

#[test]
fn usage_passes_arguments_to_the_pinned_ccusage() {
    let scratch = Scratch::new();
    link_node(&scratch);
    let config = scratch.home().join(".config/relaxkit");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("lock.toml"),
        "relaxkit = \"0.1.0\"\n\n[tools.ccusage]\nversion = \"20.0.52\"\nroute = \"npm\"\nhosts = [\"claude\"]\n",
    )
    .unwrap();

    scratch
        .relaxkit()
        .args(["usage", "daily", "--json"])
        .assert()
        .success();

    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("npx") && c.contains("ccusage@20.0.52 daily --json")),
        "{calls:?}"
    );
}

#[test]
fn usage_without_a_lock_uses_the_recipe_pin() {
    let scratch = Scratch::new();
    link_node(&scratch);

    scratch
        .relaxkit()
        .args(["usage", "daily"])
        .assert()
        .success();

    let calls = scratch.calls();
    assert!(
        calls.iter().any(|c| c.contains("ccusage@20.0.51 daily")),
        "{calls:?}"
    );
}

// ---- JSON discipline (K110) ------------------------------------------------------------------------------------

#[test]
fn doctor_json_prints_one_document_on_stdout() {
    let scratch = Scratch::claude_only();

    let output = scratch
        .relaxkit()
        .args(["--json", "doctor"])
        .output()
        .expect("doctor runs");

    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let document: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert!(document["checks"].is_array(), "{stdout}");
}

// ---- run groups (K105) -----------------------------------------------------------------------------------------

/// One logged call: its display and its environment.
type Logged = (String, Vec<(String, String)>);

/// A runner that answers the Homebrew self step and records the environment of every call.
#[derive(Clone, Default)]
struct EnvRecorder {
    calls: Arc<Mutex<Vec<Logged>>>,
}

impl relaxkit::exec::Runner for EnvRecorder {
    fn run(&self, cmd: &relaxkit::exec::Cmd) -> anyhow::Result<Output> {
        self.calls
            .lock()
            .unwrap()
            .push((cmd.to_string(), cmd.environment().to_vec()));
        if cmd.argv() == ["--version"] {
            return Ok(Output::ok("relaxkit 0.1.0\n"));
        }
        Ok(Output::ok(""))
    }

    fn which(&self, _program: &str) -> Option<PathBuf> {
        None
    }
}

#[test]
fn self_step_passes_its_run_group_to_the_tool_run() {
    // The Homebrew self step re-runs `update --tools` through the brew link. The child gets the parent's run as
    // its group (K105), so `undo` reverts the two as one.
    let root = tempfile::tempdir().unwrap();
    let recorder = EnvRecorder::default();
    let mut ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    ctx.runner = Box::new(recorder.clone());
    let parent_run = ctx.run.clone();

    let exe = Path::new("/opt/homebrew/Cellar/relaxkit/0.1.0/bin/relaxkit");
    relaxkit::selfupdate::apply_with(&ctx, exe, "untrusted comment: test", false, true).unwrap();

    let calls = recorder.calls.lock().unwrap().clone();
    let (_, env) = calls
        .iter()
        .find(|(display, _)| display.ends_with("update --tools"))
        .expect("the tool run is re-executed");
    assert!(
        env.contains(&(
            "RELAXKIT_RUN_GROUP".to_string(),
            parent_run.as_str().to_string()
        )),
        "{env:?}"
    );
}
