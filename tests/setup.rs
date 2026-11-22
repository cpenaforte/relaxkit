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
    // again. The rerun finds the probe passing and exits 0. serena's probe is used: its reinstall runs only stubs.
    let mut scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.clear_calls();
    scratch.fail_once("serena --help");

    let output = scratch.relaxkit().arg("setup").assert().success();

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(text.contains("repaired serena on claude"), "{text}");
    let calls = scratch.calls();
    assert!(
        calls.iter().any(|call| call == "serena --help"),
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("uv tool install"))
            .count(),
        1,
        "the reinstall runs the install steps once: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("claude mcp add --scope user serena")),
        "the reinstall re-runs the MCP step: {calls:?}"
    );
}

#[test]
fn a_repair_that_still_fails_leaves_a_fail_and_setup_exits_1() {
    // OWNER-3 (iv): a probe that still fails after the repair stays a fail, and setup exits 1 with that check's fix.
    let mut scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.fail_calls_matching("serena --help");

    let output = scratch.relaxkit().arg("setup").assert().code(1);

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(text.contains("repaired serena on claude"), "{text}");
    assert!(text.contains("fail tool.serena.claude"), "{text}");
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

#[test]
fn an_unparsable_claude_config_is_left_alone_and_fails_the_run() {
    // R3-3: a ~/.claude.json that does not parse is reported as unreadable, with no repair. Setup never runs
    // `claude mcp` against it, and its bytes stay as they are.
    let scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    let config = scratch.home().join(".claude.json");
    let broken = "{ not json, hand edited\n";
    fs::write(&config, broken).unwrap();
    scratch.clear_calls();

    scratch.relaxkit().arg("setup").assert().code(1);

    assert_eq!(fs::read_to_string(&config).unwrap(), broken);
    let calls = scratch.calls();
    assert!(
        calls.iter().all(|call| !call.starts_with("claude mcp")),
        "no claude mcp call may touch an unreadable config: {calls:?}"
    );
}

#[test]
fn a_listed_plugin_whose_cli_is_missing_gets_its_cli_installed_and_no_plugin_install() {
    // R3-4: a listed unpinned plugin (caveman) whose CLI probe fails gets its non-plugin install steps again, which
    // install the CLI. `claude plugin install` is never called, so the plugin's version does not move.
    let scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.without("caveman");
    scratch.clear_calls();

    // The CLI stub does not come back from the npm stub, so the rerun may still exit non-zero; only the calls matter.
    let _ = scratch.relaxkit().arg("setup").output().unwrap();

    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.contains("install -g @caveman-ai/cli@")),
        "the caveman CLI must be installed again: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.starts_with("claude plugin install")),
        "no plugin install may run for a listed plugin: {calls:?}"
    );
}

/// Writes a lock with every recipe locked on every host it names, except `skipped`, so setup's install step has
/// nothing to do for the other recipes.
fn write_lock_except(scratch: &Scratch, skipped: Option<&str>) {
    let mut lock = String::from("relaxkit = \"0.1.0\"\n");
    for (id, version, route, hosts) in LOCKED_RECIPES {
        if Some(*id) == skipped {
            continue;
        }
        lock.push_str(&format!(
            "\n[tools.{id}]\nversion = \"{version}\"\nroute = \"{route}\"\nhosts = [{hosts}]\n"
        ));
    }
    let lock_path = scratch.home().join(".config/relaxkit/lock.toml");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, lock).unwrap();
}

const LOCKED_RECIPES: &[(&str, &str, &str, &str)] = &[
    ("caveman", "4.1.0", "npm", "\"claude\", \"opencode\""),
    ("ccusage", "1.0.0", "npm", "\"claude\", \"opencode\""),
    (
        "chrome-devtools",
        "1.0.0",
        "npm",
        "\"claude\", \"opencode\"",
    ),
    (
        "claude-md-management",
        "1.0.0",
        "claude-plugin",
        "\"claude\"",
    ),
    ("frontend-design", "1.0.0", "claude-plugin", "\"claude\""),
    ("opencode-sandbox", "1.0.0", "npm", "\"opencode\""),
    ("playwright", "1.0.0", "npm", "\"claude\", \"opencode\""),
    ("rtk", "1.2.1", "script", "\"claude\", \"opencode\""),
    ("serena", "2.1.0", "uv", "\"claude\", \"opencode\""),
    (
        "superpowers",
        "5.0.1",
        "opencode-plugin",
        "\"claude\", \"opencode\"",
    ),
];

#[test]
fn setup_with_an_unreadable_opencode_list_keeps_the_user_model_and_says_the_tier_is_not_applied() {
    // K122 / F23-22: setup cannot resolve a tier without the list, so the owner's implementer model stays, and the
    // output says the tier was not applied.
    let scratch = Scratch::new();
    write_lock_except(&scratch, None);
    let config = scratch.home().join(".config/opencode/opencode.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        "{\"agents\": {\"implementer\": {\"model\": \"anthropic/claude-sonnet-5-5\"}}}\n",
    )
    .unwrap();

    let output = scratch
        .relaxkit()
        .arg("setup")
        .output()
        .expect("setup runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config).unwrap()).expect("opencode.json is JSON");
    assert_eq!(
        json["agents"]["implementer"]["model"],
        serde_json::json!("anthropic/claude-sonnet-5-5"),
        "{text}"
    );
    assert!(text.contains("tier roles were not applied"), "{text}");
}

#[test]
fn an_unreadable_opencode_plugins_entry_stops_setup_before_any_write() {
    // R6-1: an OpenCode `plugins` entry that is not a list stops setup before any OpenCode file is written. The config
    // file and AGENTS.md keep their bytes, no `opencode plugin` call runs, nothing is repaired, and the run exits 1
    // with the read error.
    let scratch = Scratch::new();
    write_lock_except(&scratch, None);
    fs::write(
        scratch.home().join(".claude.json"),
        "{\"mcpServers\": {\"serena\": {}, \"chrome-devtools\": {}, \"playwright\": {}}}\n",
    )
    .unwrap();
    let config = scratch.home().join(".config/opencode/opencode.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let broken = "{\"plugins\": \"x\", \"mcp\": {\"servers\": {\"serena\": {}, \"chrome-devtools\": {}, \"playwright\": {}}}}\n";
    fs::write(&config, broken).unwrap();
    let agents = scratch.home().join(".config/opencode/AGENTS.md");
    let notes = "# my notes\n";
    fs::write(&agents, notes).unwrap();
    scratch.clear_calls();

    let output = scratch.relaxkit().arg("setup").assert().code(1);

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).into_owned();
    assert!(stderr.contains("is not a list"), "{stderr}");
    assert!(!text.contains("repaired"), "{text}");
    assert_eq!(fs::read_to_string(&config).unwrap(), broken);
    assert_eq!(fs::read_to_string(&agents).unwrap(), notes);
    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.starts_with("opencode plugin")),
        "no opencode plugin call may touch an unreadable plugins entry: {calls:?}"
    );
}

#[test]
fn an_unreadable_opencode_plugins_entry_stops_the_install_loop_before_any_write() {
    // R7-1: setup reads the OpenCode plugins entry before its install loop. Serena is left out of the lock, so its
    // install step would remove and re-add its MCP entry in opencode.json. It does not: the file and AGENTS.md keep
    // their bytes, no `opencode mcp` call runs, and the run exits 1 on the plugins error.
    let scratch = Scratch::new();
    write_lock_except(&scratch, Some("serena"));
    let config = scratch.home().join(".config/opencode/opencode.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let broken = "{\"plugins\": \"x\", \"mcp\": {\"servers\": {\"chrome-devtools\": {}, \"playwright\": {}}}}\n";
    fs::write(&config, broken).unwrap();
    let agents = scratch.home().join(".config/opencode/AGENTS.md");
    let notes = "# my notes\n";
    fs::write(&agents, notes).unwrap();
    scratch.clear_calls();

    let output = scratch.relaxkit().arg("setup").assert().code(1);

    let stderr = String::from_utf8_lossy(&output.get_output().stderr).into_owned();
    assert!(stderr.contains("plugins in "), "{stderr}");
    assert!(stderr.contains("is not a list"), "{stderr}");
    assert_eq!(fs::read_to_string(&config).unwrap(), broken);
    assert_eq!(fs::read_to_string(&agents).unwrap(), notes);
    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.starts_with("opencode mcp") && !call.starts_with("opencode plugin")),
        "no opencode mcp or plugin call may run before the plugins entry is read: {calls:?}"
    );
}

#[test]
fn a_repair_never_moves_a_listed_unpinned_plugin() {
    // R2-3: a repair for an unpinned plugin that is already listed reruns its configuration only. It never calls
    // `claude plugin install`, which would move the version it can see.
    let mut scratch = Scratch::claude_only();
    scratch.relaxkit().arg("setup").assert().success();
    scratch.clear_calls();
    // The first `claude plugin list` of the rerun is caveman's listing check (caveman sorts first in the lock), which
    // fails once. Caveman is the listed unpinned plugin that is repaired; the plugin is still never installed.
    scratch.fail_once("claude plugin list");

    let output = scratch.relaxkit().arg("setup").assert().success();

    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(text.contains("repaired caveman on claude"), "{text}");
    let calls = scratch.calls();
    assert!(
        calls
            .iter()
            .all(|call| !call.starts_with("claude plugin install")),
        "{calls:?}"
    );
}

#[test]
fn setup_does_not_record_a_brew_or_system_gh() {
    // The Scratch PATH holds a gh stub that the pack did not install, so setup must not write a gh lock entry (K84).
    let scratch = Scratch::claude_only();

    scratch.relaxkit().arg("setup").assert().success();

    let lock =
        fs::read_to_string(scratch.home().join(".config/relaxkit/lock.toml")).unwrap_or_default();
    assert!(!lock.contains("[tools.gh]"), "{lock}");
}
