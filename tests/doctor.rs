//! `relaxkit status` and `relaxkit doctor` on fixture homes (Task 20). Most tests call the library with a fake
//! runner, a fake registry and a temporary root, and build the hosts directly, so nothing reads the real PATH,
//! the real home or the environment. The binary tests use the stub support.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use relaxkit::commands::doctor::{self, Check, CheckStatus};
use relaxkit::commands::status::{self, StatusReport};
use relaxkit::ctx::Ctx;
use relaxkit::edit::block_upsert;
use relaxkit::exec::{Cmd, FakeRunner, Output, Runner};
use relaxkit::host::claude::{ClaudeHost, claude_paths};
use relaxkit::host::opencode::{OpencodeHost, config_file_in, opencode_dir};
use relaxkit::host::{Host, HostInfo, HostKind};
use relaxkit::net::FakeNet;
use relaxkit::runtime::nvm::npx_bin;
use support::Scratch;

const CLAUDE_VERSION: &str = "2.1.330";
const OPENCODE_VERSION: &str = "2.2.6";

/// A context rooted at `root`, with the given fake runner and registry.
fn ctx(root: &Path, runner: FakeRunner, net: FakeNet) -> Ctx {
    Ctx::for_test(root, runner, net)
}

/// The Claude Code host rooted at `root`, as the host finder builds it without the environment.
fn claude(root: &Path) -> Box<dyn Host> {
    let (dir, state) = claude_paths(root, None);
    Box::new(ClaudeHost::new(
        HostInfo {
            kind: HostKind::Claude,
            binary: PathBuf::from("/usr/local/bin/claude"),
            version: CLAUDE_VERSION.to_string(),
        },
        dir,
        state,
    ))
}

/// The OpenCode host rooted at `root`.
fn opencode(root: &Path, version: &str) -> Box<dyn Host> {
    Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: version.to_string(),
        },
        opencode_dir(root, None),
    ))
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Writes `lock.toml` with the given tools (id, version, route, hosts).
fn lock(root: &Path, tools: &[(&str, &str, &str, &[&str])]) {
    let mut text = String::from("relaxkit = \"0.1.0\"\n");
    for (id, version, route, hosts) in tools {
        let names: Vec<String> = hosts.iter().map(|h| format!("\"{h}\"")).collect();
        text.push_str(&format!(
            "\n[tools.{id}]\nversion = \"{version}\"\nroute = \"{route}\"\nhosts = [{}]\n",
            names.join(", ")
        ));
    }
    write(&root.join(".config/relaxkit/lock.toml"), &text);
}

fn find<'a>(checks: &'a [Check], id: &str) -> &'a Check {
    checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check `{id}` in {checks:#?}"))
}

/// A runner that answers the sandbox self-test as a sandbox that refuses everything does.
fn refusing_sandbox(root: &Path) -> FakeRunner {
    let npx = npx_bin(&ctx(root, FakeRunner::new(), FakeNet::new()));
    FakeRunner::new().reply(
        &format!("{} --yes", npx.display()),
        Output::ok("status=1\n403\n"),
    )
}

fn status_text(report: &StatusReport) -> String {
    report.lines.join("\n")
}

// ---- doctor -------------------------------------------------------------------------------------------------

#[test]
fn doctor_reports_v1_mcp_shape_without_rewriting() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join(".config/opencode/opencode.jsonc");
    let text = "{\n  // mine\n  \"mcp\": { \"legacy\": { \"type\": \"local\", \"command\": [\"x\"] } }\n}\n";
    write(&config, text);
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let check = find(&checks, "opencode.mcp_shape");
    assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
    assert!(check.fix.is_some(), "{check:?}");
    assert_eq!(fs::read_to_string(&config).unwrap(), text);
}

#[test]
fn doctor_warns_when_anthropic_base_url_is_set() {
    // relaxkit does not set ANTHROPIC_BASE_URL, and a user's own proxy sets it too, so it is a warn with the removal
    // as its fix. Nothing repairs it: setup never deletes a variable it did not write (ruling OWNER-2).
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join(".claude/settings.json"),
        "{\"env\": {\"ANTHROPIC_BASE_URL\": \"https://example.invalid\"}}\n",
    );
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.anthropic_base_url");
    assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
    assert_eq!(
        check.message,
        "ANTHROPIC_BASE_URL is set in settings.json. relaxkit does not set it; remove it if you do not use a proxy.",
        "{check:?}"
    );
    let fix = check.fix.as_deref().unwrap_or("");
    assert!(
        fix.contains("remove the ANTHROPIC_BASE_URL key"),
        "{check:?}"
    );
}

#[test]
fn doctor_fails_if_a_status_line_was_replaced() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join(".claude/settings.json"),
        "{\"statusLine\": {\"type\": \"command\", \"command\": \"caveman-statusline\"}}\n",
    );
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.status_line");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
}

#[test]
fn doctor_leaves_an_ordinary_status_line_alone() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join(".claude/settings.json"),
        "{\"statusLine\": {\"type\": \"command\", \"command\": \"my-prompt\"}}\n",
    );
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    assert_eq!(find(&checks, "claude.status_line").status, CheckStatus::Ok);
}

#[test]
fn doctor_runs_the_sandbox_selftest_and_fails_when_a_probe_fails() {
    let root = tempfile::tempdir().unwrap();
    let npx = npx_bin(&ctx(root.path(), FakeRunner::new(), FakeNet::new()));
    // Probe 0 reports that the write outside the project succeeded, so the sandbox does not enforce.
    let leaking = FakeRunner::new().reply(
        &format!("{} --yes", npx.display()),
        Output::ok("status=0\n"),
    );
    let context = ctx(root.path(), leaking, FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.sandbox.selftest");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert!(
        check.message.contains("write outside the project"),
        "{check:?}"
    );

    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());
    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();
    assert_eq!(
        find(&checks, "claude.sandbox.selftest").status,
        CheckStatus::Ok
    );
}

#[test]
fn doctor_json_schema_is_stable() {
    let root = tempfile::tempdir().unwrap();
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());
    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let json = doctor::to_json(&checks);

    let object = json.as_object().expect("an object");
    assert_eq!(object.keys().collect::<Vec<_>>(), vec!["checks"]);
    let entries = json["checks"].as_array().expect("an array");
    assert!(!entries.is_empty());
    for entry in entries {
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["id", "status", "message", "fix"], "{entry}");
        assert!(
            matches!(entry["status"].as_str(), Some("ok" | "warn" | "fail")),
            "{entry}"
        );
    }
}

#[test]
fn doctor_flags_a_locked_tool_whose_probe_fails() {
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("rtk", "1.2.1", "script", &["claude"])]);
    let runner = refusing_sandbox(root.path()).reply(
        "rtk gain",
        Output {
            code: 1,
            stdout: String::new(),
            stderr: "no".into(),
        },
    );
    let context = ctx(root.path(), runner, FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    assert_eq!(find(&checks, "tool.rtk.claude").status, CheckStatus::Fail);
}

#[test]
fn doctor_fails_when_a_locked_mcp_command_is_missing_from_the_mcp_path() {
    // The MCP entry starts `serena` from the MCP PATH (spec 6), so a missing command means the entry cannot start.
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("serena", "2.1.0", "uv", &["claude"])]);

    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();
    let missing = find(&checks, "mcp.serena.claude");
    assert_eq!(missing.status, CheckStatus::Fail, "{missing:?}");
    assert!(missing.fix.is_some(), "{missing:?}");

    // The same command on the MCP PATH (~/.local/bin) passes.
    let command = root.path().join(".local/bin/serena");
    write(&command, "#!/bin/sh\n");
    fs::set_permissions(
        &command,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();
    assert_eq!(find(&checks, "mcp.serena.claude").status, CheckStatus::Ok);
}

#[test]
fn a_plugin_probe_that_exits_zero_without_its_plugin_fails() {
    // R3-5: a tool probe passes only when it exits 0 and its expect text is in its output. `claude plugin list` that
    // exits 0 without superpowers in its list is a fail, not an ok.
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "1.2.0", "claude-plugin", &["claude"])],
    );
    let runner = refusing_sandbox(root.path())
        .with_program("claude", Path::new("/usr/local/bin/claude"))
        .reply("claude plugin list", Output::ok("claude-md-management@x\n"))
        .reply(
            "/usr/local/bin/claude plugin list",
            Output::ok("claude-md-management@x\n"),
        );

    let checks = doctor::checks_for(
        &ctx(root.path(), runner, FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();

    let probe = find(&checks, "tool.superpowers.claude");
    assert_eq!(probe.status, CheckStatus::Fail, "{probe:?}");
}

#[test]
fn an_opencode_plugin_probe_fails_unless_the_plugin_is_listed() {
    // R4-2: an OpenCode plugin probe passes only when its program exits 0 and the plugin is listed under `plugins` in
    // the OpenCode config. Without superpowers listed the probe fails; with it listed the probe passes.
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "5.0.1", "opencode-plugin", &["opencode"])],
    );
    let runner = || {
        refusing_sandbox(root.path())
            .with_program("opencode", Path::new("/usr/local/bin/opencode"))
            .reply(
                "/usr/local/bin/opencode --version",
                Output::ok("opencode 2.2.6\n"),
            )
    };
    let config = config_file_in(&opencode_dir(root.path(), None));
    write(&config, "{\"plugins\": [\"opencode-sandbox@1.0.0\"]}\n");

    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let probe = find(&checks, "tool.superpowers.opencode");
    assert_eq!(probe.status, CheckStatus::Fail, "{probe:?}");
    assert!(
        probe
            .message
            .contains("superpowers is not listed in the opencode config"),
        "{probe:?}"
    );

    write(
        &config,
        "{\"plugins\": [\"superpowers@git+https://github.com/obra/superpowers.git#v5.0.1\"]}\n",
    );
    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let probe = find(&checks, "tool.superpowers.opencode");
    assert_eq!(probe.status, CheckStatus::Ok, "{probe:?}");
}

#[test]
fn an_unreadable_opencode_plugins_entry_is_a_plugin_config_fail_and_is_not_rewritten() {
    // R5-1: an OpenCode `plugins` entry that is not a list cannot be read. The plugin probe reports that under its own
    // id, plugin-config.<id>.<kind>, and the file keeps its bytes.
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "5.0.1", "opencode-plugin", &["opencode"])],
    );
    let runner = refusing_sandbox(root.path())
        .with_program("opencode", Path::new("/usr/local/bin/opencode"))
        .reply(
            "/usr/local/bin/opencode --version",
            Output::ok("opencode 2.2.6\n"),
        );
    let config = config_file_in(&opencode_dir(root.path(), None));
    let broken = "{\"plugins\": \"x\"}\n";
    write(&config, broken);

    let checks = doctor::checks_for(
        &ctx(root.path(), runner, FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();

    let check = find(&checks, "plugin-config.superpowers.opencode");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert_eq!(fs::read_to_string(&config).unwrap(), broken);
}

#[test]
fn caveman_on_opencode_fails_unless_its_plugin_entry_is_listed() {
    // R5-2: caveman's OpenCode plugin entry `./plugins/caveman` is checked like the other plugins. The probe passes
    // only when that entry is listed under `plugins`.
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("caveman", "4.1.0", "npm", &["opencode"])]);
    let runner = || {
        refusing_sandbox(root.path())
            .with_program("caveman", Path::new("/usr/local/bin/caveman"))
            .reply("/usr/local/bin/caveman status", Output::ok("caveman ok\n"))
    };
    let config = config_file_in(&opencode_dir(root.path(), None));
    write(&config, "{\"plugins\": [\"opencode-sandbox@1.0.0\"]}\n");

    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let probe = find(&checks, "tool.caveman.opencode");
    assert_eq!(probe.status, CheckStatus::Fail, "{probe:?}");
    assert!(
        probe
            .message
            .contains("./plugins/caveman is not listed in the opencode config"),
        "{probe:?}"
    );

    write(&config, "{\"plugins\": [\"./plugins/caveman\"]}\n");
    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let probe = find(&checks, "tool.caveman.opencode");
    assert_eq!(probe.status, CheckStatus::Ok, "{probe:?}");
}

#[test]
fn opencode_tuple_plugin_entries_are_read_by_their_first_element() {
    // R6-2: OpenCode accepts `[name, options]` in `plugins`. The first element is the plugin spec. A tuple whose first
    // element is not a string cannot be read, so it is a plugin-config Fail.
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "5.0.1", "opencode-plugin", &["opencode"])],
    );
    let runner = || {
        refusing_sandbox(root.path())
            .with_program("opencode", Path::new("/usr/local/bin/opencode"))
            .reply(
                "/usr/local/bin/opencode --version",
                Output::ok("opencode 2.2.6\n"),
            )
    };
    let config = config_file_in(&opencode_dir(root.path(), None));
    write(
        &config,
        "{\"plugins\": [[\"superpowers@git+https://github.com/obra/superpowers.git#v5.0.1\", {}]]}\n",
    );

    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let probe = find(&checks, "tool.superpowers.opencode");
    assert_eq!(probe.status, CheckStatus::Ok, "{probe:?}");

    write(&config, "{\"plugins\": [[1, {}]]}\n");
    let checks = doctor::checks_for(
        &ctx(root.path(), runner(), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let check = find(&checks, "plugin-config.superpowers.opencode");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
}

#[test]
fn status_reads_a_git_plugin_spec_version_from_its_tag() {
    // R8-1: a git spec `superpowers@git+https://...#v7.0.2` is at version 7.0.2, so status says "as locked" and
    // prints no drift.
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "7.0.2", "opencode-plugin", &["opencode"])],
    );
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        "{\"plugins\": [\"superpowers@git+https://github.com/obra/superpowers.git#v7.0.2\"]}\n",
    );
    let context = ctx(root.path(), FakeRunner::new(), FakeNet::new());

    let report = status::report_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let text = status_text(&report);
    assert!(
        text.contains(
            "superpowers on opencode: installed 7.0.2, locked 7.0.2 (opencode-plugin); as locked"
        ),
        "{text}"
    );
}

#[test]
fn doctor_runs_a_probe_from_the_pack_bin_by_its_full_path() {
    // The probe's program lives only in relaxkit's bin directory, which the stub PATH does not hold. The engine finds it
    // there, so doctor must run it by its full path rather than by its bare name (ruling Important 6).
    let root = tempfile::tempdir().unwrap();
    let program = ctx(root.path(), FakeRunner::new(), FakeNet::new())
        .dirs
        .bin
        .join("rtk");
    write(&program, "#!/bin/sh\n");
    fs::set_permissions(
        &program,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    lock(root.path(), &[("rtk", "1.2.1", "script", &["claude"])]);
    let runner = refusing_sandbox(root.path()).reply(
        &format!("{} gain", program.display()),
        Output::ok("rtk 1.2.1\n"),
    );

    let checks = doctor::checks_for(
        &ctx(root.path(), runner, FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();

    let probe = find(&checks, "tool.rtk.claude");
    assert_eq!(probe.status, CheckStatus::Ok, "{probe:?}");
}

#[test]
fn doctor_fails_when_the_sandbox_selftest_cannot_fetch_its_runtime() {
    // A fetch or start failure is not a refusal: the self-test did not run, so the check fails (ruling Important 4).
    let root = tempfile::tempdir().unwrap();
    let npx = npx_bin(&ctx(root.path(), FakeRunner::new(), FakeNet::new()));
    let runner = FakeRunner::new().reply(
        &format!("{} --yes", npx.display()),
        Output::fail(1, "npm ERR! code ENOTFOUND"),
    );

    let checks = doctor::checks_for(
        &ctx(root.path(), runner, FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();

    let check = find(&checks, "claude.sandbox.selftest");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert!(check.message.contains("could not run"), "{check:?}");
}

#[test]
fn doctor_warns_when_a_probe_is_not_run() {
    // Playwright's probe starts with `{npx}`, which doctor does not run. Not running it is a warn, never an ok
    // (ruling Important item 6).
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("playwright", "0.0.86", "npm", &["claude"])]);

    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();

    let probe = find(&checks, "tool.playwright.claude");
    assert_eq!(probe.status, CheckStatus::Warn, "{probe:?}");
    assert!(probe.message.contains("not run"), "{probe:?}");
}

#[test]
fn doctor_fails_when_a_locked_mcp_entry_is_missing_from_claude() {
    // R2-1 (OWNER-3 (c)): the locked serena entry must exist in Claude's user-scope state file. The command is present,
    // so only the entry is missing; setup re-adds it.
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("serena", "2.1.0", "uv", &["claude"])]);
    let command = root.path().join(".local/bin/serena");
    write(&command, "#!/bin/sh\n");
    fs::set_permissions(
        &command,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();
    let entry = find(&checks, "mcp-entry.serena.claude");
    assert_eq!(entry.status, CheckStatus::Fail, "{entry:?}");
    assert!(
        entry
            .fix
            .as_deref()
            .unwrap_or("")
            .contains("run relaxkit setup"),
        "{entry:?}"
    );

    write(
        &root.path().join(".claude.json"),
        "{\"mcpServers\": {\"serena\": {}}}\n",
    );
    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[claude(root.path())],
    )
    .unwrap();
    assert_eq!(
        find(&checks, "mcp-entry.serena.claude").status,
        CheckStatus::Ok
    );
}

#[test]
fn doctor_fails_when_a_locked_mcp_entry_is_missing_from_opencode() {
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("serena", "2.1.0", "uv", &["opencode"])]);
    let command = root.path().join(".local/bin/serena");
    write(&command, "#!/bin/sh\n");
    fs::set_permissions(
        &command,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let config = config_file_in(&opencode_dir(root.path(), None));
    write(&config, "{\"mcp\": {\"servers\": {}}}\n");

    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    let entry = find(&checks, "mcp-entry.serena.opencode");
    assert_eq!(entry.status, CheckStatus::Fail, "{entry:?}");

    write(&config, "{\"mcp\": {\"servers\": {\"serena\": {}}}}\n");
    let checks = doctor::checks_for(
        &ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new()),
        &[opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();
    assert_eq!(
        find(&checks, "mcp-entry.serena.opencode").status,
        CheckStatus::Ok
    );
}

/// A runner that answers a probe only when the child's PATH holds `dir` (ruling R2-4).
struct NeedsDirOnPath {
    dir: PathBuf,
    answer: &'static str,
}

impl Runner for NeedsDirOnPath {
    fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
        let path = cmd
            .environment()
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        if path.split(':').any(|entry| Path::new(entry) == self.dir) {
            Ok(Output::ok(self.answer))
        } else {
            Ok(Output::fail(127, "not found"))
        }
    }

    fn which(&self, _program: &str) -> Option<PathBuf> {
        None
    }
}

#[test]
fn doctor_probes_run_on_the_engine_search_path() {
    // R2-4: a probe child gets the engine's search path, which starts with the Node link bin, so a fresh machine's
    // probe finds its own Node. The program is in the pack bin, which the stub PATH does not hold.
    let root = tempfile::tempdir().unwrap();
    let base = ctx(root.path(), FakeRunner::new(), FakeNet::new());
    let program = base.dirs.bin.join("rtk");
    write(&program, "#!/bin/sh\n");
    fs::set_permissions(
        &program,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let node_link = base.dirs.data.join("node").join("bin");
    lock(root.path(), &[("rtk", "1.2.1", "script", &["claude"])]);
    let mut context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());
    context.runner = Box::new(NeedsDirOnPath {
        dir: node_link,
        answer: "rtk 1.2.1\n",
    });

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let probe = find(&checks, "tool.rtk.claude");
    assert_eq!(probe.status, CheckStatus::Ok, "{probe:?}");
}

#[test]
fn doctor_dry_run_warns_that_the_selftests_did_not_run() {
    // R2-5: a dry run does not run the sandbox self-test, so each self-test id is a warn, never an ok.
    let root = tempfile::tempdir().unwrap();
    let mut context = ctx(root.path(), FakeRunner::new(), FakeNet::new());
    context.dry_run = true;

    let checks = doctor::checks_for(
        &context,
        &[claude(root.path()), opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();

    for id in ["claude.sandbox.selftest", "opencode.sandbox.selftest"] {
        let check = find(&checks, id);
        assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
        assert!(check.message.contains("not run in a dry run"), "{check:?}");
    }
}

#[test]
fn selftest_runs_its_runtime_on_the_engine_search_path() {
    // R3-1: the self-test's npx child gets the engine's search path, so the pinned runtime is found through the Node
    // link on a fresh machine. The runner refuses the runtime unless the child's PATH holds the link.
    let root = tempfile::tempdir().unwrap();
    let node_link = ctx(root.path(), FakeRunner::new(), FakeNet::new())
        .dirs
        .data
        .join("node")
        .join("bin");
    let mut context = ctx(root.path(), FakeRunner::new(), FakeNet::new());
    context.runner = Box::new(NeedsDirOnPath {
        dir: node_link,
        answer: "status=1\n403\n",
    });

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.sandbox.selftest");
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}

#[test]
fn doctor_exits_nonzero_when_a_check_fails() {
    let scratch = Scratch::claude_only();
    fs::create_dir_all(scratch.home().join(".claude")).unwrap();
    fs::write(
        scratch.home().join(".claude/settings.json"),
        "{\"env\": {\"ANTHROPIC_BASE_URL\": \"https://example.invalid\"}}\n",
    )
    .unwrap();

    let output = scratch
        .relaxkit()
        .args(["doctor", "--json"])
        .assert()
        .code(1);
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("JSON on stdout");
    assert!(
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["status"] == "fail")
    );
}

// ---- status -------------------------------------------------------------------------------------------------

#[test]
fn status_lists_installed_versus_locked_versions_and_flags_drift() {
    let root = tempfile::tempdir().unwrap();
    lock(root.path(), &[("rtk", "1.2.1", "script", &["claude"])]);
    let runner = FakeRunner::new()
        .with_program("rtk", Path::new("/usr/local/bin/rtk"))
        .reply("/usr/local/bin/rtk --version", Output::ok("rtk 1.2.0\n"))
        .reply("claude plugin list", Output::ok(""));
    // A managed block whose body was edited by hand.
    let claude_md = block_upsert("# notes\n", "roles", "edited by hand");
    write(&root.path().join(".claude/CLAUDE.md"), &claude_md);
    let context = ctx(root.path(), runner, FakeNet::new());

    let report = status::report_for(&context, &[claude(root.path())]).unwrap();

    let text = status_text(&report);
    assert!(
        text.contains("rtk") && text.contains("1.2.0") && text.contains("1.2.1"),
        "{text}"
    );
    assert!(
        text.contains("drift") && text.contains("CLAUDE.md"),
        "{text}"
    );
}

#[test]
fn status_marks_each_role_advisory_or_enforced_per_host() {
    let root = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new().reply(
        "/usr/local/bin/opencode models",
        Output::ok(
            "anthropic/claude-haiku-5-5\nanthropic/claude-sonnet-5-5\nanthropic/claude-opus-5-5\n",
        ),
    );
    let context = ctx(root.path(), runner, FakeNet::new());

    let report = status::report_for(
        &context,
        &[claude(root.path()), opencode(root.path(), OPENCODE_VERSION)],
    )
    .unwrap();

    let text = status_text(&report);
    let line = |role: &str| {
        text.lines()
            .find(|l| l.starts_with(role))
            .unwrap_or_else(|| panic!("no line for {role} in:\n{text}"))
            .to_string()
    };
    let implementer = line("implementer");
    assert!(
        implementer.contains("claude haiku (advisory)"),
        "{implementer}"
    );
    assert!(
        implementer.contains("opencode anthropic/claude-haiku-5-5 (enforced per agent)"),
        "{implementer}"
    );
    let planner = line("planner");
    assert!(
        planner.contains("(advisory)") && !planner.contains("enforced"),
        "{planner}"
    );
}

#[test]
fn status_flags_a_host_too_old_for_a_tool() {
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("superpowers", "7.0.2", "plugin", &["opencode"])],
    );
    let context = ctx(root.path(), FakeRunner::new(), FakeNet::new());

    let report = status::report_for(&context, &[opencode(root.path(), "2.0.3")]).unwrap();

    let text = status_text(&report);
    assert!(
        text.contains("superpowers") && text.contains("2.0.3") && text.contains("2.2.6"),
        "{text}"
    );
    assert!(text.contains("older than"), "{text}");
}

#[test]
fn status_lists_other_node_installs_and_the_rtk_gain_line() {
    let root = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new()
        .with_program("node", Path::new("/usr/local/bin/node"))
        .reply("rtk gain", Output::ok("Total saved: 12 KB\n"));
    let context = ctx(root.path(), runner, FakeNet::new());

    let text = status_text(&status::report_for(&context, &[]).unwrap());
    assert!(text.contains("/usr/local/bin/node"), "{text}");
    assert!(text.contains("rtk gain: Total saved: 12 KB"), "{text}");

    let context = ctx(root.path(), FakeRunner::new(), FakeNet::new());
    let text = status_text(&status::report_for(&context, &[]).unwrap());
    assert!(text.contains("rtk gain: n/a"), "{text}");
}

#[test]
fn doctor_fails_on_a_settings_file_that_does_not_parse() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join(".claude/settings.json");
    write(&settings, "{\"env\": {\n");
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.settings");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), "{\"env\": {\n");
}

// ---- the OpenCode sandbox plugin (spec 10 item 25) ----------------------------------------------------------------

const RELAXKIT_OPENCODE_LISTED: &str = "{\"plugins\": [\"relaxkit-opencode@git+https://github.com/cpenaforte/relaxkit.git#v0.1.0::path:adapters/opencode-plugin\"]}\n";

const SANDBOX_RUNTIME_REGISTRY: &str = "https://registry.npmjs.org/@anthropic-ai%2fsandbox-runtime";

const RELAXKIT_LATEST_RELEASE: &str =
    "https://api.github.com/repos/cpenaforte/relaxkit/releases/latest";

/// The date `days` before today, as the registries write a release time.
fn released_days_ago(days: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (year, month, day) = civil_from_days(now.div_euclid(86_400) - days);
    format!("{year:04}-{month:02}-{day:02}T00:00:00.000Z")
}

/// Civil date of a day count since 1970-01-01 (the proleptic Gregorian calendar).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The network the plugin check reads. The bundled runtime 0.0.83 was released 200 days ago; when `newer` is given, 0.0.84
/// was released that many days ago. relaxkit's newest release was `release_days` days ago.
fn health_network(newer: Option<i64>, release_days: i64) -> FakeNet {
    let mut versions = vec![("0.0.83".to_string(), released_days_ago(200))];
    if let Some(days) = newer {
        versions.push(("0.0.84".to_string(), released_days_ago(days)));
    }
    let listed: String = versions
        .iter()
        .map(|(version, _)| format!("\"{version}\": {{}}"))
        .collect::<Vec<_>>()
        .join(", ");
    let times: String = versions
        .iter()
        .map(|(version, at)| format!("\"{version}\": \"{at}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(r#"{{"versions": {{{listed}}}, "time": {{{times}}}}}"#);
    FakeNet::new().with(SANDBOX_RUNTIME_REGISTRY, &body).with(
        RELAXKIT_LATEST_RELEASE,
        &format!(
            r#"{{"published_at": "{}"}}"#,
            released_days_ago(release_days)
        ),
    )
}

#[test]
fn doctor_fails_when_the_opencode_plugin_is_not_listed() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        "{\"plugins\": []}\n",
    );
    let context = ctx(root.path(), FakeRunner::new(), health_network(None, 3));

    let checks = doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let check = find(&checks, "opencode.sandbox.plugin");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert!(
        check.message.contains("not in the OpenCode plugins list"),
        "{check:?}"
    );
}

#[test]
fn doctor_shows_a_listed_plugin_with_its_runtime_lag_and_release_age_as_ok() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        RELAXKIT_OPENCODE_LISTED,
    );
    let context = ctx(root.path(), FakeRunner::new(), health_network(Some(10), 3));

    let checks = doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let check = find(&checks, "opencode.sandbox.plugin");
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
    assert!(
        check
            .message
            .contains("sandbox-runtime 0.0.83 bundled, 1 release behind"),
        "{check:?}"
    );
    assert!(
        check
            .message
            .contains("relaxkit's newest release is 3 days old"),
        "{check:?}"
    );
}

#[test]
fn doctor_warns_only_after_120_days_without_a_relaxkit_release() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        RELAXKIT_OPENCODE_LISTED,
    );
    let status_at = |days: i64| {
        let context = ctx(root.path(), FakeRunner::new(), health_network(None, days));
        find(
            &doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap(),
            "opencode.sandbox.plugin",
        )
        .status
    };

    assert_eq!(status_at(119), CheckStatus::Ok);
    assert_eq!(status_at(121), CheckStatus::Warn);
}

#[test]
fn doctor_never_warns_on_a_runtime_lag_alone() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        RELAXKIT_OPENCODE_LISTED,
    );
    // The runtime has been out of date for 150 days, but relaxkit released three days ago.
    let context = ctx(root.path(), FakeRunner::new(), health_network(Some(150), 3));

    let checks = doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let check = find(&checks, "opencode.sandbox.plugin");
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}

#[test]
fn doctor_treats_an_unreachable_registry_as_unknown_not_a_failure() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        RELAXKIT_OPENCODE_LISTED,
    );
    let context = ctx(root.path(), FakeRunner::new(), FakeNet::new());

    let checks = doctor::checks_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let check = find(&checks, "opencode.sandbox.plugin");
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
    assert!(check.message.contains("health unknown"), "{check:?}");
    assert!(
        check.message.contains("release age is unknown"),
        "{check:?}"
    );
}

#[test]
fn status_shows_the_opencode_plugin_with_its_runtime_lag_and_release_age() {
    let root = tempfile::tempdir().unwrap();
    write(
        &config_file_in(&opencode_dir(root.path(), None)),
        RELAXKIT_OPENCODE_LISTED,
    );
    let context = ctx(root.path(), FakeRunner::new(), health_network(Some(10), 3));

    let report = status::report_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let text = status_text(&report);
    assert!(
        text.contains("the relaxkit-opencode plugin is listed"),
        "{text}"
    );
    assert!(
        text.contains("sandbox-runtime 0.0.83 bundled, 1 release behind"),
        "{text}"
    );
    assert!(
        text.contains("relaxkit's newest release is 3 days old"),
        "{text}"
    );
}
