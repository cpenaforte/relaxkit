//! `relaxkit setup` against stub host commands in a temporary HOME (Task 19, rulings K46 to K54).

mod support;

use std::fs;
use std::path::PathBuf;

use predicates::prelude::*;
use relaxkit::config::KitConfig;
use relaxkit::ctx::Ctx;
use relaxkit::exec::FakeRunner;
use relaxkit::net::FakeNet;
use relaxkit::platform::KitDirs;
use relaxkit::presets::Preset;
use relaxkit::state::undo;
use support::{Scratch, tree};

const STEP_NAMES: [&str; 7] = [
    "detect",
    "prerequisites",
    "install",
    "configure",
    "sandbox",
    "login",
    "verify",
];

/// Host commands the stubs answer. A line starting with one of these names is a call to a host or tool.
const TOOLS: [&str; 12] = [
    "claude", "opencode", "gh", "brew", "uv", "rtk", "npm", "npx", "serena", "caveman", "sudo",
    "nvm",
];

/// Argument words that change something on the machine. A call with one of them is mutating.
const MUTATING_WORDS: [&str; 7] = [
    "install",
    "add",
    "remove",
    "init",
    "telemetry",
    "marketplace",
    "routing",
];

fn is_mutating(call: &str) -> bool {
    let mut words = call.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    TOOLS.contains(&program) && words.any(|word| MUTATING_WORDS.contains(&word))
}

fn position_of(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is missing from:\n{text}"))
}

#[test]
fn setup_dry_run_prints_the_plan_and_writes_nothing() {
    let scratch = Scratch::claude_only();
    let before = tree(scratch.home());

    let output = scratch
        .relaxkit()
        .args(["setup", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    let positions: Vec<usize> = STEP_NAMES
        .iter()
        .map(|step| position_of(&text, &format!("{step}:")))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert_eq!(
        tree(scratch.home()),
        before,
        "a dry run changed the home tree"
    );
    let calls = scratch.calls();
    assert!(
        calls.iter().all(|call| call.ends_with(" --version")),
        "a dry run ran more than host detection: {calls:?}"
    );
}

#[test]
fn detect_runs_before_any_install() {
    let scratch = Scratch::claude_only();

    scratch.relaxkit().arg("setup").assert().success();

    let calls = scratch.calls();
    assert_eq!(calls.first().map(String::as_str), Some("claude --version"));
    let first_install = calls
        .iter()
        .position(|call| call.contains(" install") || call.contains(" add "))
        .expect("an install call");
    assert!(first_install > 0, "{calls:?}");
}

#[test]
fn setup_runs_steps_in_spec_order() {
    let scratch = Scratch::claude_only();

    let output = scratch.relaxkit().arg("setup").assert().success();
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let positions: Vec<usize> = STEP_NAMES
        .iter()
        .map(|step| position_of(&text, &format!("{step}:")))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
}

#[test]
fn setup_is_idempotent() {
    let scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    let first = tree(scratch.home());
    scratch.clear_calls();

    scratch.relaxkit().arg("setup").assert().success();

    let calls = scratch.calls();
    let mutating: Vec<&String> = calls.iter().filter(|call| is_mutating(call)).collect();
    assert!(mutating.is_empty(), "a second setup mutated: {mutating:?}");
    let second = tree(scratch.home());
    let data = scratch.home().join(".local");
    let config_only = |files: &std::collections::BTreeMap<PathBuf, Vec<u8>>| {
        files
            .iter()
            .filter(|(path, _)| !path.starts_with(&data))
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(config_only(&second), config_only(&first));
}

#[test]
fn setup_with_only_claude_skips_opencode_steps() {
    let scratch = Scratch::claude_only();

    scratch.relaxkit().arg("setup").assert().success();

    let calls = scratch.calls();
    assert!(
        calls.iter().all(|call| !call.starts_with("opencode ")),
        "{calls:?}"
    );
}

#[test]
fn setup_with_only_opencode_runs_no_claude_step() {
    // Caveman's OpenCode files come from GitHub, which this test does not reach, so the run is expected to stop
    // there. The check is that no Claude Code step ran first.
    let scratch = Scratch::new();
    scratch.without("claude");

    scratch
        .relaxkit()
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicate::str::contains("caveman"));

    let calls = scratch.calls();
    assert!(
        calls.iter().all(|call| !call.starts_with("claude ")),
        "{calls:?}"
    );
}

#[test]
fn setup_with_neither_host_exits_nonzero_with_a_hint() {
    let scratch = Scratch::new();
    scratch.without("claude");
    scratch.without("opencode");

    scratch
        .relaxkit()
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Claude Code").and(predicate::str::contains("OpenCode")));
}

#[test]
fn setup_asks_no_questions() {
    let scratch = Scratch::claude_only();

    scratch.relaxkit().arg("setup").assert().code(0);
}

#[test]
fn interrupted_setup_is_undoable() {
    let mut scratch = Scratch::claude_only();
    let home = scratch.home().to_path_buf();
    let settings = home.join(".claude/settings.json");
    let claude_md = home.join(".claude/CLAUDE.md");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "{\"theme\": \"dark\"}\n").unwrap();
    fs::write(&claude_md, "# my notes\n").unwrap();
    // The rtk probe runs after configure and sandbox have written the host files, so the run stops after those
    // edits are journaled.
    scratch.fail_calls_matching("rtk gain");

    scratch.relaxkit().arg("setup").assert().failure();
    assert_ne!(
        fs::read_to_string(&settings).unwrap(),
        "{\"theme\": \"dark\"}\n",
        "the interrupted run should have edited settings.json before it stopped"
    );

    let ctx = Ctx::for_test(&home, FakeRunner::new(), FakeNet::new());
    undo(&ctx, None).expect("undo");

    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        "{\"theme\": \"dark\"}\n"
    );
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), "# my notes\n");
}

#[test]
fn setup_applies_the_balanced_preset_by_default() {
    let scratch = Scratch::claude_only();

    scratch.relaxkit().arg("setup").assert().success();

    let home = scratch.home();
    // A fresh machine has no config.toml, since a missing file reads as the defaults, which are balanced.
    let preset = KitConfig::load(&KitDirs::rooted(home)).unwrap().preset;
    assert_eq!(preset, Preset::Balanced);
    let caveman = fs::read_to_string(home.join(".config/caveman/config.json")).unwrap();
    assert!(
        caveman.contains("\"defaultMode\": \"caveman\""),
        "{caveman}"
    );
    let settings = fs::read_to_string(home.join(".claude/settings.json")).unwrap();
    assert!(
        settings.contains("SUPERPOWERS_DISABLE_TELEMETRY"),
        "{settings}"
    );
    assert!(!settings.contains("autoCompactWindow"), "{settings}");
    let claude_md = fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("opus"), "{claude_md}");
    assert!(claude_md.contains("haiku"), "{claude_md}");
    assert!(claude_md.contains("sonnet"), "{claude_md}");
}

#[test]
fn setup_keeps_an_existing_preset() {
    let scratch = Scratch::claude_only();
    let config_dir = scratch.home().join(".config/relaxkit");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), "preset = \"careful\"\n").unwrap();

    scratch.relaxkit().arg("setup").assert().success();

    let config = fs::read_to_string(config_dir.join("config.toml")).unwrap();
    assert!(config.contains("preset = \"careful\""), "{config}");
}

#[test]
fn setup_exits_3_when_the_login_needs_the_owner() {
    let mut scratch = Scratch::claude_only();
    scratch.gh_auth_status(1);

    let output = scratch
        .relaxkit()
        .arg("setup")
        .assert()
        .code(3)
        .get_output()
        .clone();

    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        text.contains("/gh auth login --web --clipboard && ") && text.contains("gh auth setup-git"),
        "{text}"
    );
    assert!(
        text.contains("verify:"),
        "the steps after login must still run:\n{text}"
    );
    assert!(
        scratch
            .calls()
            .iter()
            .all(|call| !call.contains("gh auth login")),
        "setup must not start the web login without a terminal"
    );
}

#[test]
fn setup_ends_by_running_doctor() {
    let scratch = Scratch::claude_only();

    let output = scratch.relaxkit().arg("setup").assert().success();
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let last = text.lines().last().unwrap_or_default();
    assert!(
        last.starts_with("verify:") && last.contains("doctor ran"),
        "{text}"
    );
    assert!(
        scratch
            .calls()
            .iter()
            .any(|call| call.contains("@anthropic-ai/sandbox-runtime")),
        "the doctor's sandbox self-test did not run"
    );
}

#[test]
fn setup_exits_nonzero_when_a_check_fails() {
    // caveman's badge replacing the status line is a fail with no repair, so the run must exit 1 and print its fix.
    let scratch = Scratch::claude_only();
    let settings = scratch.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        "{\"statusLine\": {\"type\": \"command\", \"command\": \"caveman-badge\"}}\n",
    )
    .unwrap();

    scratch
        .relaxkit()
        .arg("setup")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("fail claude.status_line"));
}

#[test]
fn setup_warns_on_anthropic_base_url_and_never_deletes_it() {
    // A warn does not fail setup, and setup never removes a variable it did not write (ruling OWNER-2).
    let scratch = Scratch::claude_only();
    let settings = scratch.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        "{\"env\": {\"ANTHROPIC_BASE_URL\": \"https://example.invalid\"}}\n",
    )
    .unwrap();

    let output = scratch.relaxkit().arg("setup").assert().code(0);
    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();

    assert!(text.contains("warn claude.anthropic_base_url"), "{text}");
    let after = fs::read_to_string(&settings).unwrap();
    assert!(
        after.contains("\"ANTHROPIC_BASE_URL\": \"https://example.invalid\""),
        "{after}"
    );
}

#[test]
fn setup_reinstalls_a_tool_whose_probe_fails_and_passes_on_rerun() {
    // OWNER-3 (i): a locked tool whose probe fails on a rerun is reinstalled at its locked version and configured
    // again. The rerun finds the probe passing and exits 0.
    let mut scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.clear_calls();
    scratch.fail_once("rtk gain");

    let output = scratch.relaxkit().arg("setup").assert().success();

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(text.contains("repaired rtk on claude"), "{text}");
    let calls = scratch.calls();
    assert!(calls.iter().any(|call| call == "rtk gain"), "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == "rtk init -g")
            .count(),
        1,
        "the reinstall runs the install steps once: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| call == "rtk telemetry disable"),
        "{calls:?}"
    );
}

#[test]
fn a_repair_that_still_fails_leaves_a_fail_and_setup_exits_1() {
    // OWNER-3 (iv): a probe that still fails after the repair stays a fail, and setup exits 1 with that check's fix.
    let mut scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.fail_calls_matching("rtk gain");

    let output = scratch.relaxkit().arg("setup").assert().code(1);

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(text.contains("repaired rtk on claude"), "{text}");
    assert!(text.contains("fail tool.rtk.claude"), "{text}");
    assert!(text.contains("run `relaxkit setup` again"), "{text}");
}

#[test]
fn setup_rewrites_a_hand_edited_relaxkit_key() {
    // OWNER-3 (ii): a relaxkit-owned key changed by hand is written back to its expected value on a rerun.
    let scratch = Scratch::claude_only();
    let settings = scratch.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        "{\"env\": {\"SUPERPOWERS_DISABLE_TELEMETRY\": \"0\"}}\n",
    )
    .unwrap();

    scratch.relaxkit().arg("setup").assert().success();

    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        value["env"]["SUPERPOWERS_DISABLE_TELEMETRY"], "1",
        "{value}"
    );
}

#[test]
fn a_failed_check_takes_precedence_over_a_pending_login() {
    // Ruling D5: a failed check (exit 1) wins over the login that needs the owner (exit 3), and the login line is
    // still printed.
    let mut scratch = Scratch::claude_only();
    scratch.gh_auth_status(1);
    let settings = scratch.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        "{\"statusLine\": {\"type\": \"command\", \"command\": \"caveman-badge\"}}\n",
    )
    .unwrap();

    let output = scratch.relaxkit().arg("setup").assert().code(1);

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned()
        + &String::from_utf8_lossy(&output.get_output().stderr);
    assert!(text.contains("fail claude.status_line"), "{text}");
    assert!(text.contains("gh auth login --web --clipboard"), "{text}");
}

#[test]
fn setup_readds_a_missing_mcp_entry_through_the_host_step() {
    // R2-1: a locked MCP entry that is gone from the host config is added again by the host's MCP step. The tool is
    // not reinstalled.
    let scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    fs::write(
        scratch.home().join(".claude.json"),
        "{\"mcpServers\": {}}\n",
    )
    .unwrap();
    scratch.clear_calls();

    let output = scratch.relaxkit().arg("setup").assert().success();

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(
        text.contains("repaired serena on claude: re-added"),
        "{text}"
    );
    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("claude mcp add --scope user serena")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.starts_with("uv tool install")),
        "the tool must not be reinstalled: {calls:?}"
    );
    let state = fs::read_to_string(scratch.home().join(".claude.json")).unwrap();
    assert!(state.contains("\"serena\""), "{state}");
}
