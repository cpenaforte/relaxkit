//! `relaxkit status` and `relaxkit doctor` on fixture homes (Task 20). Most tests call the library with a fake
//! runner, a fake registry and a temporary root, and build the hosts directly, so nothing reads the real PATH,
//! the real home or the environment. The binary tests use the stub support.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use relaxkit::commands::doctor::{self, Check, CheckStatus};
use relaxkit::commands::status::{self, StatusReport};
use relaxkit::ctx::Ctx;
use relaxkit::edit::block_upsert;
use relaxkit::exec::{FakeRunner, Output};
use relaxkit::host::claude::{ClaudeHost, claude_paths};
use relaxkit::host::opencode::{OpencodeHost, opencode_dir};
use relaxkit::host::{Host, HostInfo, HostKind};
use relaxkit::net::FakeNet;
use relaxkit::runtime::nvm::npx_bin;
use support::Scratch;

const CLAUDE_VERSION: &str = "2.1.330";
const OPENCODE_VERSION: &str = "2.2.6";
const SANDBOX_RUNTIME: &str = "https://registry.npmjs.org/@anthropic-ai%2fsandbox-runtime";
const SANDBOX_PLUGIN: &str = "https://registry.npmjs.org/opencode-sandbox";

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
fn doctor_flags_a_project_sandbox_file() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write(
        &project.join(".opencode/sandbox.json"),
        "{\"mode\": \"permissive\"}\n",
    );
    let mut context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());
    context.project = Some(project);

    let checks = doctor::checks_for(&context, &[]).unwrap();

    let check = find(&checks, "project.sandbox_file");
    assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
}

#[test]
fn doctor_fails_if_caveman_routing_changed_anthropic_base_url() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join(".claude/settings.json"),
        "{\"env\": {\"ANTHROPIC_BASE_URL\": \"https://example.invalid\"}}\n",
    );
    let context = ctx(root.path(), refusing_sandbox(root.path()), FakeNet::new());

    let checks = doctor::checks_for(&context, &[claude(root.path())]).unwrap();

    let check = find(&checks, "claude.anthropic_base_url");
    assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
    assert!(
        check
            .fix
            .as_deref()
            .unwrap_or("")
            .contains("caveman off routing"),
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

/// The date `days` before today, as the npm registry writes a release time.
fn released_days_ago(days: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
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

/// A registry that serves the pinned plugin, released `days` ago, with the runtime 0.0.76 to 0.0.83 published.
fn sandbox_registry(days: i64) -> FakeNet {
    let plugin = format!(
        "{{\"dist-tags\":{{\"latest\":\"0.7.0\"}},\"time\":{{\"0.7.0\":\"{}\"}},\"versions\":{{\"0.7.0\":{{\"dependencies\":{{\"@anthropic-ai/sandbox-runtime\":\"^0.0.76\"}}}}}}}}",
        released_days_ago(days)
    );
    let versions: Vec<String> = (76..=83)
        .map(|patch| format!("\"0.0.{patch}\":{{}}"))
        .collect();
    let runtime = format!("{{\"versions\":{{{}}}}}", versions.join(","));
    FakeNet::new()
        .with(SANDBOX_PLUGIN, &plugin)
        .with(SANDBOX_RUNTIME, &runtime)
}

#[test]
fn doctor_warns_only_after_120_days_without_a_plugin_release() {
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("opencode-sandbox", "0.7.0", "npm", &["opencode"])],
    );
    let hosts = [opencode(root.path(), OPENCODE_VERSION)];

    let at_119 = ctx(
        root.path(),
        refusing_sandbox(root.path()),
        sandbox_registry(119),
    );
    let checks = doctor::checks_for(&at_119, &hosts).unwrap();
    assert_eq!(
        find(&checks, "plugin.opencode-sandbox").status,
        CheckStatus::Ok
    );

    // Runtime lag alone never warns: 7 releases behind, released 3 days ago.
    let lagging = ctx(
        root.path(),
        refusing_sandbox(root.path()),
        sandbox_registry(3),
    );
    let checks = doctor::checks_for(&lagging, &hosts).unwrap();
    assert_eq!(
        find(&checks, "plugin.opencode-sandbox").status,
        CheckStatus::Ok
    );

    let at_121 = ctx(
        root.path(),
        refusing_sandbox(root.path()),
        sandbox_registry(121),
    );
    let checks = doctor::checks_for(&at_121, &hosts).unwrap();
    let check = find(&checks, "plugin.opencode-sandbox");
    assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
    assert!(
        check.fix.as_deref().unwrap_or("").contains("wrapper"),
        "{check:?}"
    );
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
        .reply("rtk --version", Output::ok("rtk 1.2.0\n"))
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
fn status_shows_sandbox_plugin_age_and_runtime_lag() {
    let root = tempfile::tempdir().unwrap();
    lock(
        root.path(),
        &[("opencode-sandbox", "0.7.0", "npm", &["opencode"])],
    );
    let context = ctx(root.path(), FakeRunner::new(), sandbox_registry(3));

    let report = status::report_for(&context, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();

    let text = status_text(&report);
    assert!(
        text.contains("opencode-sandbox 0.7.0 (released 3d ago; bundles sandbox-runtime 0.0.76, 7 releases behind)"),
        "{text}"
    );

    let unreachable = ctx(root.path(), FakeRunner::new(), FakeNet::new());
    let report =
        status::report_for(&unreachable, &[opencode(root.path(), OPENCODE_VERSION)]).unwrap();
    assert!(
        status_text(&report).contains("health unknown"),
        "{}",
        status_text(&report)
    );
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
