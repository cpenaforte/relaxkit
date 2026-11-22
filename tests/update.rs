//! `relaxkit update` for the tools (Task 21, rulings K63 to K82). Every test runs against a temporary root, a scripted
//! runner that records each command with its environment, and FakeNet. No test touches the real home, the real
//! network, or a real host command.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use relaxkit::commands::update::{RowResult, UpdateArgs, UpdateReport, run_with_hosts, to_json};
use relaxkit::ctx::{Ctx, Reporter};
use relaxkit::exec::{Cmd, Output, Runner};
use relaxkit::host::claude::ClaudeHost;
use relaxkit::host::opencode::OpencodeHost;
use relaxkit::host::{Host, HostInfo, HostKind};
use relaxkit::net::FakeNet;
use relaxkit::platform::{Arch, KitDirs, Os, current_arch, current_os};
use relaxkit::runtime::nvm::NvmSearch;
use relaxkit::state::lockfile::{LockedTool, Lockfile};
use relaxkit::state::{RunId, journal, undo};
use relaxkit::tools::engine::Engine;
use support::{Scratch, tree};

type Answer = Arc<dyn Fn(&Cmd) -> Output + Send + Sync>;
type Hook = Arc<dyn Fn(&Cmd) + Send + Sync>;

#[derive(Default)]
struct State {
    /// Answers by display prefix. The first match wins; a command with no match is an error, as in FakeRunner.
    answers: Vec<(String, Answer)>,
    cmds: Vec<Cmd>,
    programs: BTreeMap<String, PathBuf>,
    /// Run after a successful command whose display starts with the prefix; they may write files, as a CLI would.
    effects: Vec<(String, Hook)>,
    /// Run before every command, so a test can check what was on disk when a change started.
    before: Vec<Hook>,
}

/// A scripted runner that keeps every command with its environment. Clones share state.
#[derive(Clone, Default)]
struct Shell(Arc<Mutex<State>>);

impl Shell {
    fn new() -> Shell {
        Shell::default()
    }

    /// Answers commands whose display starts with `prefix` with `out`.
    fn on(&self, prefix: &str, out: Output) {
        self.answer(prefix, Arc::new(move |_| out.clone()));
    }

    /// Answers with a function, so a later command can change what an earlier one reads back.
    fn answer(&self, prefix: &str, f: Answer) {
        self.0.lock().unwrap().answers.push((prefix.to_string(), f));
    }

    fn effect(&self, prefix: &str, f: Hook) {
        self.0.lock().unwrap().effects.push((prefix.to_string(), f));
    }

    fn before(&self, f: Hook) {
        self.0.lock().unwrap().before.push(f);
    }

    fn program(&self, name: &str, path: &Path) {
        self.0
            .lock()
            .unwrap()
            .programs
            .insert(name.to_string(), path.to_path_buf());
    }

    fn cmds(&self) -> Vec<Cmd> {
        self.0.lock().unwrap().cmds.clone()
    }

    /// The argv of every command asked for, in order, without the program name.
    fn argvs(&self) -> Vec<Vec<String>> {
        self.cmds().iter().map(|cmd| cmd.argv().to_vec()).collect()
    }

    /// Display strings of every command asked for, in order.
    fn lines(&self) -> Vec<String> {
        self.cmds().iter().map(|cmd| cmd.to_string()).collect()
    }
}

impl Runner for Shell {
    fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
        let display = cmd.to_string();
        let (before, answer, effects) = {
            let mut state = self.0.lock().unwrap();
            state.cmds.push(cmd.clone());
            let answer = state
                .answers
                .iter()
                .find(|(prefix, _)| display.starts_with(prefix.as_str()))
                .map(|(_, answer)| answer.clone());
            let effects: Vec<Hook> = state
                .effects
                .iter()
                .filter(|(prefix, _)| display.starts_with(prefix.as_str()))
                .map(|(_, hook)| hook.clone())
                .collect();
            (state.before.clone(), answer, effects)
        };
        for hook in &before {
            hook(cmd);
        }
        let Some(answer) = answer else {
            anyhow::bail!("unexpected command in test: {display}");
        };
        let out = answer(cmd);
        if out.code == 0 {
            for hook in &effects {
                hook(cmd);
            }
        }
        Ok(out)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.0.lock().unwrap().programs.get(program).cloned()
    }
}

/// A temporary root with a Node 24 link, a fake nvm, the Claude host, and scripted commands and network.
struct Fixture {
    root: tempfile::TempDir,
    shell: Shell,
    net: FakeNet,
    search: NvmSearch,
    hosts: Vec<Box<dyn Host>>,
    /// The run every command of this fixture writes under, so a test can undo it.
    run: RunId,
}

const NODE_OLD: &str = "v24.13.0";

impl Fixture {
    fn new() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(root.path());
        let nvm_dir = root.path().join(".nvm");
        fs::create_dir_all(nvm_dir.join("versions/node").join(NODE_OLD).join("bin")).unwrap();
        fs::write(
            nvm_dir
                .join("versions/node")
                .join(NODE_OLD)
                .join("bin/node"),
            "",
        )
        .unwrap();
        fs::write(nvm_dir.join("nvm.sh"), "# fake nvm\n").unwrap();
        fs::create_dir_all(&dirs.data).unwrap();
        std::os::unix::fs::symlink(
            nvm_dir.join("versions/node").join(NODE_OLD),
            dirs.data.join("node"),
        )
        .unwrap();
        let search = NvmSearch {
            nvm_dir: Some(nvm_dir),
            xdg_config_home: None,
            brew_prefixes: Vec::new(),
            zdotdir: None,
            installer_sha256: String::new(),
        };
        let claude = ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.330".to_string(),
            },
            root.path().join(".claude"),
            root.path().join(".claude.json"),
        );
        Fixture {
            root,
            shell: Shell::new(),
            net: FakeNet::new(),
            search,
            hosts: vec![Box::new(claude)],
            run: RunId::new(),
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            dirs: KitDirs::rooted(self.root()),
            runner: Box::new(self.shell.clone()),
            net: Box::new(self.net.clone()),
            out: Reporter::capture(),
            dry_run: false,
            project: None,
            allow_sudo: false,
            run: self.run.clone(),
        }
    }

    fn serve(&self, url: &str, body: &str) {
        self.net.clone().with(url, body);
    }

    /// Writes the lock with the given entries: (tool, version, route, hosts).
    fn lock(&self, entries: &[(&str, &str, &str, &[&str])]) {
        let mut tools = BTreeMap::new();
        for (tool, version, route, hosts) in entries {
            tools.insert(
                tool.to_string(),
                LockedTool {
                    version: version.to_string(),
                    route: route.to_string(),
                    hosts: hosts.iter().map(|h| h.to_string()).collect(),
                    host_versions: BTreeMap::new(),
                },
            );
        }
        let lock = Lockfile {
            relaxkit: "0.1.0".to_string(),
            tools,
        };
        lock.save(&self.ctx(), &RunId::new()).unwrap();
    }

    fn read_lock(&self) -> Lockfile {
        Lockfile::load(&self.ctx()).unwrap().expect("a lock")
    }

    fn npx(&self) -> String {
        self.root()
            .join(".local/share/relaxkit/node/bin/npx")
            .display()
            .to_string()
    }

    fn npm(&self) -> String {
        self.root()
            .join(".local/share/relaxkit/node/bin/npm")
            .display()
            .to_string()
    }

    fn run(&self, check: bool, tool: Option<&str>) -> anyhow::Result<UpdateReport> {
        let ctx = self.ctx();
        let args = UpdateArgs {
            check,
            tool: tool.map(str::to_string),
        };
        run_with_hosts(&ctx, &args, &self.hosts, &self.search)
    }

    /// The probe answer a Playwright or Chrome DevTools MCP entry gets at `version`.
    fn mcp_probe(&self, package: &str, version: &str) -> String {
        format!("{} -y {package}@{version} --help", self.npx())
    }
}

fn row<'a>(report: &'a UpdateReport, tool: &str) -> &'a relaxkit::commands::update::UpdateRow {
    report
        .rows
        .iter()
        .find(|row| row.tool == tool)
        .unwrap_or_else(|| panic!("no row for {tool} in {:?}", report.rows))
}

/// Every command that changes something, by the same test the rest of the suite uses (a host or tool word).
fn mutating(line: &str) -> bool {
    const WORDS: [&str; 9] = [
        "install",
        "add",
        "remove",
        "init",
        "telemetry",
        "marketplace",
        "update",
        "upgrade",
        "--force",
    ];
    line.split_whitespace().any(|word| WORDS.contains(&word))
}

#[test]
fn check_reports_installed_and_newest_and_changes_nothing() {
    let fx = Fixture::new();
    fx.lock(&[
        ("playwright", "0.0.86", "npm", &["claude"]),
        (
            "superpowers",
            "7.0.2",
            "claude-plugin:anthropic-plugin-directory",
            &["claude"],
        ),
        ("rtk", "1.1.0", "brew", &["claude"]),
    ]);
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.2"}"#,
    );
    // No runner answer and no served registry for rtk: its row is "newest unknown", not a failure of the run (K70).
    let before = tree(fx.root());

    let report = fx.run(true, None).unwrap();

    assert!(
        fx.shell.cmds().is_empty(),
        "a check ran {:?}",
        fx.shell.lines()
    );
    assert_eq!(tree(fx.root()), before, "a check changed a file");
    let playwright = row(&report, "playwright");
    assert_eq!(playwright.installed.as_deref(), Some("0.0.86"));
    assert_eq!(playwright.newest.as_deref(), Some("0.0.87"));
    assert_eq!(playwright.result, RowResult::Available);
    assert_eq!(row(&report, "superpowers").result, RowResult::Current);
    let rtk = row(&report, "rtk");
    assert_eq!(rtk.newest, None);
    assert_eq!(rtk.result, RowResult::NewestUnknown);
    assert!(report.failed.is_empty());
    assert!(report.restart.is_empty());

    let json = to_json(&report);
    let first = &json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["tool"] == "playwright")
        .unwrap();
    assert_eq!(first["installed"], "0.0.86");
    assert_eq!(first["newest"], "0.0.87");
}

#[test]
fn each_tool_updates_by_its_own_route() {
    let fx = Fixture::new();
    fx.lock(&[
        (
            "superpowers",
            "7.0.2",
            "claude-plugin:anthropic-plugin-directory",
            &["claude"],
        ),
        ("caveman", "4.1.1", "npm", &["claude"]),
        ("playwright", "0.0.86", "npm", &["claude"]),
        ("serena", "2.1.0", "uv", &["claude"]),
        ("ccusage", "20.0.51", "npm", &["claude"]),
    ]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );
    fx.serve(
        "https://registry.npmjs.org/@caveman-ai%2fcli/latest",
        r#"{"version":"4.2.0"}"#,
    );
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    fx.serve(
        "https://pypi.org/pypi/serena-agent/json",
        r#"{"info":{"version":"2.2.0"}}"#,
    );
    fx.serve(
        "https://registry.npmjs.org/ccusage/latest",
        r#"{"version":"20.0.52"}"#,
    );
    let npx = fx.npx();
    let npm = fx.npm();
    fx.shell.program("uv", Path::new("/usr/local/bin/uv"));
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    Registry::new(
        &[("superpowers", "7.0.2"), ("caveman", "4.1.1")],
        &[("superpowers", "7.0.3"), ("caveman", "4.2.0")],
    )
    .attach(&fx.shell);
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    fx.shell
        .on("claude plugin update caveman@caveman", Output::ok(""));
    fx.shell.on(
        &format!("{npm} install -g @caveman-ai/cli@4.2.0"),
        Output::ok(""),
    );
    fx.shell.on("caveman off routing --yes", Output::ok(""));
    fx.shell.on("caveman telemetry off", Output::ok(""));
    fx.shell.on("caveman status", Output::ok("ok\n"));
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell
        .on("claude mcp remove --scope user playwright", Output::ok(""));
    fx.shell
        .on("claude mcp add --scope user playwright", Output::ok(""));
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::ok("ok\n"),
    );
    fx.shell.on(
        "uv tool install -p 3.13 serena-agent==2.2.0 --force",
        Output::ok(""),
    );
    fx.shell.on("serena init", Output::ok(""));
    fx.shell.on("serena --help", Output::ok("usage\n"));
    fx.shell.on(
        &format!("{npx} -y ccusage@20.0.52 --offline --help"),
        Output::ok("usage\n"),
    );

    let report = fx.run(false, None).unwrap();

    let lines = fx.shell.lines();
    let has = |line: &str| lines.iter().any(|l| l == line);
    // Claude Code plugins: marketplace first, then the plugin (K71).
    let marketplace = lines
        .iter()
        .position(|l| l == "claude plugin marketplace update")
        .unwrap();
    let update = lines
        .iter()
        .position(|l| l == "claude plugin update superpowers@anthropic-plugin-directory")
        .unwrap();
    assert!(marketplace < update, "{lines:?}");
    assert!(has("claude plugin update caveman@caveman"), "{lines:?}");
    assert!(has("caveman off routing --yes"), "{lines:?}");
    assert!(has("caveman telemetry off"), "{lines:?}");
    // npm pins are rewritten through the MCP remove and add, at the exact new version (K71).
    assert!(
        has("claude mcp remove --scope user playwright"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("claude mcp add --scope user playwright ")
                && l.ends_with(&format!(
                    "-- {npx} -y @playwright/mcp@0.0.87 --browser chromium --isolated"
                ))),
        "{lines:?}"
    );
    // Serena reinstalls at the newest pin with --force (spike I6), then runs `serena init` (K71).
    assert!(
        has("uv tool install -p 3.13 serena-agent==2.2.0 --force"),
        "{lines:?}"
    );
    assert!(has("serena init"), "{lines:?}");
    // ccusage keeps its offline argv at the new pin.
    assert!(
        has(&format!("{npx} -y ccusage@20.0.52 --offline --help")),
        "{lines:?}"
    );

    for tool in ["superpowers", "caveman", "playwright", "serena", "ccusage"] {
        assert!(
            matches!(row(&report, tool).result, RowResult::Updated),
            "{tool}: {:?}",
            row(&report, tool)
        );
    }
    let lock = fx.read_lock();
    assert_eq!(lock.tools["superpowers"].version, "7.0.3");
    assert_eq!(lock.tools["caveman"].version, "4.2.0");
    assert_eq!(lock.tools["playwright"].version, "0.0.87");
    assert_eq!(lock.tools["serena"].version, "2.2.0");
    assert_eq!(lock.tools["ccusage"].version, "20.0.52");
    assert_eq!(lock.tools["playwright"].route, "npm");
    assert_eq!(lock.tools["serena"].route, "uv");
}

#[test]
fn opencode_superpowers_moves_its_tag_by_remove_then_add() {
    let mut fx = Fixture::new();
    let config = fx.root().join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = "superpowers@git+https://github.com/obra/superpowers.git#v7.0.2";
    fs::write(&config, format!("{{\"plugins\": [\"{old}\"]}}\n")).unwrap();
    fx.hosts = vec![Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        fx.root().join(".config/opencode"),
    ))];
    fx.lock(&[("superpowers", "7.0.2", "opencode-plugin", &["opencode"])]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );
    fx.shell.on("opencode plugin remove", Output::ok(""));
    fx.shell.on("opencode plugin add", Output::ok(""));
    fx.shell.on("opencode --version", Output::ok("2.2.6\n"));
    // The effect matches the command's own argv, not its display string: the display quotes the `+` and `#` of a git spec,
    // so a display prefix would never match. An add writes the spec it was given, as OpenCode does.
    let cfg = config.clone();
    fx.shell.effect(
        "opencode plugin add",
        Arc::new(move |cmd| {
            let spec = cmd.argv().last().cloned().unwrap_or_default();
            fs::write(&cfg, format!("{{\"plugins\": [\"{spec}\"]}}\n")).unwrap();
        }),
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    let argvs = fx.shell.argvs();
    let remove = vec!["plugin".to_string(), "remove".to_string(), old.to_string()];
    let add = vec![
        "plugin".to_string(),
        "add".to_string(),
        "superpowers@git+https://github.com/obra/superpowers.git#v7.0.3".to_string(),
    ];
    let at = |argv: &Vec<String>| argvs.iter().position(|a| a == argv);
    assert!(at(&remove).is_some() && at(&add).is_some(), "{argvs:?}");
    assert!(
        at(&remove) < at(&add),
        "the old entry must go before the new one is added: {argvs:?}"
    );
    assert_eq!(row(&report, "superpowers").result, RowResult::Updated);
    assert_eq!(fx.read_lock().tools["superpowers"].version, "7.0.3");
}

#[test]
fn failed_probe_rolls_back_only_that_tool() {
    let fx = Fixture::new();
    fx.lock(&[
        ("playwright", "0.0.86", "npm", &["claude"]),
        ("caveman", "4.1.1", "npm", &["claude"]),
    ]);
    let before_entry = fx.read_lock().tools["playwright"].clone();
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    fx.serve(
        "https://registry.npmjs.org/@caveman-ai%2fcli/latest",
        r#"{"version":"4.2.0"}"#,
    );
    let npx = fx.npx();
    let npm = fx.npm();
    Registry::new(&[("caveman", "4.1.1")], &[("caveman", "4.2.0")]).attach(&fx.shell);
    fx.shell
        .on("claude plugin update caveman@caveman", Output::ok(""));
    fx.shell.on(
        &format!("{npm} install -g @caveman-ai/cli@4.2.0"),
        Output::ok(""),
    );
    fx.shell.on("caveman off routing --yes", Output::ok(""));
    fx.shell.on("caveman telemetry off", Output::ok(""));
    fx.shell.on("caveman status", Output::ok("ok\n"));
    fx.shell
        .on("claude mcp remove --scope user playwright", Output::ok(""));
    fx.shell
        .on("claude mcp add --scope user playwright", Output::ok(""));
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.86 install-browser chromium"),
        Output::ok(""),
    );
    // The new Playwright fails its probe; the old one passes (its rollback).
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::fail(1, "broken"),
    );
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.86"),
        Output::ok("ok\n"),
    );

    let report = fx.run(false, None).unwrap();

    assert_eq!(row(&report, "playwright").result, RowResult::RolledBack);
    assert_eq!(report.failed, vec!["playwright".to_string()]);
    assert!(matches!(row(&report, "caveman").result, RowResult::Updated));
    let lines = fx.shell.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("claude mcp add --scope user playwright ")
                && l.ends_with(&format!(
                    "-- {npx} -y @playwright/mcp@0.0.86 --browser chromium --isolated"
                ))),
        "the locked entry was not put back: {lines:?}"
    );
    let lock = fx.read_lock();
    assert_eq!(
        lock.tools["playwright"], before_entry,
        "a rolled-back entry must stay byte-identical"
    );
    assert_eq!(lock.tools["caveman"].version, "4.2.0");
}

#[test]
fn sandbox_plugin_moves_only_after_the_selftest_passes() {
    let fx_setup = |self_test_output: &'static str| {
        let mut fx = Fixture::new();
        let config = fx.root().join(".config/opencode/opencode.jsonc");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "{\"plugins\": [\"opencode-sandbox@0.7.0\"]}\n").unwrap();
        fx.hosts = vec![Box::new(OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            fx.root().join(".config/opencode"),
        ))];
        fx.lock(&[(
            "opencode-sandbox",
            "0.7.0",
            "opencode-plugin",
            &["opencode"],
        )]);
        fx.serve(
            "https://registry.npmjs.org/opencode-sandbox/latest",
            r#"{"version":"0.7.1"}"#,
        );
        serve_sandbox_candidate(&fx, "^0.0.76", &["0.0.76"]);
        let npx = fx.npx();
        fx.shell.on(
            &format!("{npx} --yes @anthropic-ai/sandbox-runtime@0.0.76"),
            Output::ok(self_test_output),
        );
        fx.shell.on("opencode plugin remove", Output::ok(""));
        fx.shell.on("opencode plugin add", Output::ok(""));
        fx.shell.on("opencode --version", Output::ok("2.2.6\n"));
        let cfg = config.clone();
        fx.shell.effect(
            "opencode plugin add opencode-sandbox@0.7.1",
            Arc::new(move |_| {
                fs::write(&cfg, "{\"plugins\": [\"opencode-sandbox@0.7.1\"]}\n").unwrap();
            }),
        );
        fx
    };

    // A self-test that lets the sandbox run an attempt (status 0) fails, so the locked plugin stays.
    let failing = fx_setup("status=0\n");
    let report = failing.run(false, Some("opencode-sandbox")).unwrap();
    let lines = failing.shell.lines();
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("opencode plugin add opencode-sandbox@0.7.1")),
        "the plugin moved before a passing self-test: {lines:?}"
    );
    assert_eq!(report.failed, vec!["opencode-sandbox".to_string()]);
    assert_eq!(
        failing.read_lock().tools["opencode-sandbox"].version,
        "0.7.0"
    );

    // Refusals (status 7 and HTTP 403) pass the self-test, so the plugin moves after it.
    let passing = fx_setup("403\nstatus=7\n");
    let report = passing.run(false, Some("opencode-sandbox")).unwrap();
    let lines = passing.shell.lines();
    let selftest = lines
        .iter()
        .position(|l| {
            l.starts_with(&format!(
                "{} --yes @anthropic-ai/sandbox-runtime@0.0.76",
                passing.npx()
            ))
        })
        .unwrap();
    let moved = lines
        .iter()
        .position(|l| l == "opencode plugin add opencode-sandbox@0.7.1")
        .unwrap();
    assert!(selftest < moved, "{lines:?}");
    assert_eq!(row(&report, "opencode-sandbox").result, RowResult::Updated);
    assert_eq!(
        passing.read_lock().tools["opencode-sandbox"].version,
        "0.7.1"
    );
    assert!(report.failed.is_empty());
}

#[test]
fn tool_not_installed_by_the_pack_is_reported_not_touched() {
    // gh from Homebrew: the pack wrote no lock entry for it, so nothing runs (K64).
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.87", "npm", &["claude"])]);

    let report = fx.run(false, Some("gh")).unwrap();

    assert!(fx.shell.cmds().is_empty(), "{:?}", fx.shell.lines());
    assert!(matches!(
        row(&report, "gh").result,
        RowResult::NotTouched(_)
    ));
    assert!(report.failed.is_empty());
}

#[test]
fn node_stays_inside_24x() {
    let fx = Fixture::new();
    fx.lock(&[]);
    fx.serve(
        "https://nodejs.org/dist/index.json",
        r#"[{"version":"v26.0.0"},{"version":"v25.1.0"},{"version":"v24.22.3"},{"version":"v24.13.0"}]"#,
    );
    let new_root = fx.root().join(".nvm/versions/node/v24.22.3");
    fs::create_dir_all(new_root.join("bin")).unwrap();
    fs::write(new_root.join("bin/node"), "").unwrap();
    fx.shell.on(
        "bash -c",
        Output::ok("Now using node v24.22.3 (npm v11.6.0)\n"),
    );
    fx.shell.on(
        &format!("{} --version", new_root.join("bin/node").display()),
        Output::ok("v24.22.3\n"),
    );

    let report = fx.run(false, Some("node")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("nvm install 25") && !l.contains("nvm install 26")),
        "{lines:?}"
    );
    let install = fx
        .shell
        .cmds()
        .into_iter()
        .find(|cmd| cmd.to_string().starts_with("bash -c"))
        .expect("the nvm install ran");
    assert!(install.to_string().contains("nvm install 24"), "{install}");
    assert!(
        install
            .environment()
            .contains(&("PROFILE".to_string(), "/dev/null".to_string())),
        "{:?}",
        install.environment()
    );
    assert_eq!(row(&report, "node").newest.as_deref(), Some("24.22.3"));
    assert!(matches!(row(&report, "node").result, RowResult::Updated));
    let link = fs::read_link(fx.root().join(".local/share/relaxkit/node")).unwrap();
    assert_eq!(link, new_root);
}

#[test]
fn undo_points_the_node_link_back_at_the_previous_node() {
    let fx = Fixture::new();
    fx.lock(&[]);
    fx.serve(
        "https://nodejs.org/dist/index.json",
        r#"[{"version":"v24.22.3"},{"version":"v24.13.0"}]"#,
    );
    let new_root = fx.root().join(".nvm/versions/node/v24.22.3");
    fs::create_dir_all(new_root.join("bin")).unwrap();
    fs::write(new_root.join("bin/node"), "").unwrap();
    fx.shell.on(
        "bash -c",
        Output::ok("Now using node v24.22.3 (npm v11.6.0)\n"),
    );
    fx.shell.on(
        &format!("{} --version", new_root.join("bin/node").display()),
        Output::ok("v24.22.3\n"),
    );
    let link = fx.root().join(".local/share/relaxkit/node");

    fx.run(false, Some("node")).unwrap();
    assert_eq!(fs::read_link(&link).unwrap(), new_root);
    undo(&fx.ctx(), Some(&fx.run)).unwrap();

    assert_eq!(
        fs::read_link(&link).unwrap(),
        fx.root().join(".nvm/versions/node/v24.13.0"),
        "undo did not put the Node link back"
    );
}

#[test]
fn undo_puts_back_the_pack_rtk_link_an_update_removed() {
    let fx = Fixture::new();
    fx.lock(&[("rtk", "1.1.0", "script", &["claude"])]);
    serve_rtk(&fx);
    let pack_rtk = fx.root().join(".local/share/relaxkit/bin/rtk");
    let elsewhere = fx.root().join("elsewhere/rtk");
    fs::create_dir_all(pack_rtk.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &pack_rtk).unwrap();
    fx.shell.program("rtk", Path::new("/opt/homebrew/bin/rtk"));
    fx.shell.on(
        &format!("{} gain", pack_rtk.display()),
        Output::ok("gain\n"),
    );
    fx.shell.on("rtk init -g", Output::ok(""));
    fx.shell.on("rtk telemetry disable", Output::ok(""));

    fx.run(false, Some("rtk")).unwrap();
    assert!(
        !pack_rtk
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    undo(&fx.ctx(), Some(&fx.run)).unwrap();

    let meta = fs::symlink_metadata(&pack_rtk).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "undo did not restore the link"
    );
    assert_eq!(fs::read_link(&pack_rtk).unwrap(), elsewhere);
}

#[test]
fn repair_re_adds_the_mcp_entry_at_the_locked_version_not_the_pin() {
    // The recipe pins Playwright 0.0.86, but the lock records 0.0.87 on this host. A repair puts back what the lock
    // says (K67, OWNER-3), and the lock itself is not rewritten by a repair.
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.87", "npm", &["claude"])]);
    let before = fx.read_lock();
    fx.shell.on("claude mcp remove", Output::ok(""));
    fx.shell.on("claude mcp add", Output::ok(""));
    let recipes = relaxkit::recipe::load_all().unwrap();
    let playwright = recipes.iter().find(|r| r.id == "playwright").unwrap();
    assert_eq!(
        playwright.pin.as_deref(),
        Some("0.0.86"),
        "the fixture expects the 0.0.86 pin"
    );
    let ctx = fx.ctx();
    let node = relaxkit::runtime::nvm::current_node(&ctx).unwrap();
    let engine = Engine {
        ctx: &ctx,
        run: &ctx.run,
        hosts: &fx.hosts,
        recipes: &recipes,
        node: &node,
    };

    engine.readd_mcp(playwright, HostKind::Claude).unwrap();

    let lines = fx.shell.lines();
    let add = lines
        .iter()
        .find(|l| l.starts_with("claude mcp add"))
        .unwrap_or_else(|| panic!("no MCP entry was added: {lines:?}"));
    assert!(add.contains("@playwright/mcp@0.0.87"), "{add}");
    assert!(!add.contains("0.0.86"), "{add}");
    assert_eq!(fx.read_lock(), before, "a repair rewrote the lock");
}

#[test]
fn rtk_below_its_minimum_host_is_not_touched_and_its_lock_stays() {
    // Claude 2.1.300 is below rtk's minimum (2.1.330), so no host is updated: the row is "not touched", and the
    // lock keeps its route and version (the update must not record a move that did not happen).
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    fx.hosts = vec![Box::new(ClaudeHost::new(
        HostInfo {
            kind: HostKind::Claude,
            binary: PathBuf::from("/usr/local/bin/claude"),
            version: "2.1.300".to_string(),
        },
        root.join(".claude"),
        root.join(".claude.json"),
    ))];
    fx.lock(&[("rtk", "1.1.0", "script", &["claude"])]);
    let before = fx.read_lock().tools["rtk"].clone();
    serve_rtk(&fx);

    let report = fx.run(false, Some("rtk")).unwrap();

    let rtk = row(&report, "rtk");
    assert!(matches!(rtk.result, RowResult::NotTouched(_)), "{rtk:?}");
    assert_eq!(fx.read_lock().tools["rtk"], before, "the lock moved");
    assert!(report.failed.is_empty(), "{report:?}");
}

#[test]
fn a_failed_claude_update_still_restarts_claude() {
    // The plugin is updated on disk before its probe fails. The probe finds no superpowers listing, so the update is
    // not confirmed, but Claude's plugin changed and must be listed to restart (K69).
    let fx = Fixture::new();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.on("claude plugin list", Output::ok(""));
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    fx.lock(&[(
        "superpowers",
        "7.0.2",
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    assert_eq!(report.failed, vec!["superpowers".to_string()], "{report:?}");
    assert!(
        report.restart.contains(&HostKind::Claude),
        "a changed Claude plugin was not listed to restart: {report:?}"
    );
}

#[test]
fn an_opencode_update_the_config_does_not_show_is_rolled_back_and_named() {
    // `opencode plugin add` exits 0, but the config still lists the old spec. The new version is not confirmed, so the
    // lock must not move, the tool goes back to its locked spec, and the report names it (K67, R4-2).
    let mut fx = Fixture::new();
    let config = fx.root().join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = "superpowers@git+https://github.com/obra/superpowers.git#v7.0.2";
    fs::write(&config, format!("{{\"plugins\": [\"{old}\"]}}\n")).unwrap();
    fx.hosts = vec![Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        fx.root().join(".config/opencode"),
    ))];
    fx.lock(&[("superpowers", "7.0.2", "opencode-plugin", &["opencode"])]);
    let before = fx.read_lock();
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );
    fx.shell.on("opencode plugin remove", Output::ok(""));
    fx.shell.on("opencode plugin add", Output::ok(""));
    fx.shell.on("opencode --version", Output::ok("2.2.6\n"));

    let report = fx.run(false, Some("superpowers")).unwrap();

    assert!(
        matches!(row(&report, "superpowers").result, RowResult::RolledBack),
        "{:?}",
        row(&report, "superpowers")
    );
    assert_eq!(report.failed, vec!["superpowers".to_string()]);
    assert_eq!(
        fx.read_lock().tools["superpowers"],
        before.tools["superpowers"],
        "the lock moved to a version the config does not show"
    );
}

#[test]
fn report_ends_with_the_restart_list() {
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.86", "npm", &["claude"])]);
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    let npx = fx.npx();
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell
        .on("claude mcp remove --scope user playwright", Output::ok(""));
    fx.shell
        .on("claude mcp add --scope user playwright", Output::ok(""));
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::ok("ok\n"),
    );

    let report = fx.run(false, Some("playwright")).unwrap();

    assert_eq!(report.restart, vec![HostKind::Claude]);
    let lines = relaxkit::commands::update::lines(&report);
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("restart:") && line.contains("Claude Code")),
        "{lines:?}"
    );
}

#[test]
fn flags_select_scope() {
    // A single tool name touches that tool only: superpowers' marketplace is never refreshed.
    let fx = Fixture::new();
    fx.lock(&[
        (
            "superpowers",
            "7.0.2",
            "claude-plugin:anthropic-plugin-directory",
            &["claude"],
        ),
        ("playwright", "0.0.86", "npm", &["claude"]),
    ]);
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    let npx = fx.npx();
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell
        .on("claude mcp remove --scope user playwright", Output::ok(""));
    fx.shell
        .on("claude mcp add --scope user playwright", Output::ok(""));
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::ok("ok\n"),
    );

    let report = fx.run(false, Some("playwright")).unwrap();
    assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
    assert!(
        fx.shell
            .lines()
            .iter()
            .all(|l| !l.contains("superpowers") && !l.contains("marketplace")),
        "{:?}",
        fx.shell.lines()
    );

    let unknown = fx.run(false, Some("no-such-tool"));
    assert!(unknown.is_err());
}

#[test]
fn backup_and_lock_snapshot_precede_any_change() {
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.86", "npm", &["claude"])]);
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    let npx = fx.npx();
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell
        .on("claude mcp remove --scope user playwright", Output::ok(""));
    fx.shell
        .on("claude mcp add --scope user playwright", Output::ok(""));
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::ok("ok\n"),
    );

    // Before the first mutating command, the lock snapshot and the lock's journal record must already exist.
    let root = fx.root().to_path_buf();
    let early = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = early.clone();
    fx.shell.before(Arc::new(move |cmd| {
        if !mutating(&cmd.to_string()) {
            return;
        }
        let dirs = KitDirs::rooted(&root);
        let snapshot_dirs: Vec<PathBuf> = fs::read_dir(&dirs.backups)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        let has_snapshot = snapshot_dirs
            .iter()
            .any(|dir| dir.join("lock.toml").is_file());
        let journaled = journal::read(&dirs)
            .map(|records| records.iter().any(|r| r.file.ends_with("lock.toml")))
            .unwrap_or(false);
        if !(has_snapshot && journaled) {
            seen.lock().unwrap().push(cmd.to_string());
        }
    }));

    let report = fx.run(false, Some("playwright")).unwrap();

    assert!(
        fx.shell.lines().iter().any(|l| mutating(l)),
        "no mutating call ran"
    );
    assert_eq!(
        *early.lock().unwrap(),
        Vec::<String>::new(),
        "a change ran before its backup"
    );
    assert!(matches!(
        row(&report, "playwright").result,
        RowResult::Updated
    ));
}

#[test]
fn rtk_is_always_latest_and_a_brew_copy_is_never_upgraded() {
    let fx = Fixture::new();
    fx.lock(&[("rtk", "1.1.0", "brew", &["claude"])]);
    fx.shell.program("rtk", Path::new("/opt/homebrew/bin/rtk"));
    fx.serve(
        "https://api.github.com/repos/rtk-ai/rtk/releases/latest",
        r#"{"tag_name":"v1.2.1"}"#,
    );
    serve_rtk(&fx);
    let pack_rtk = fx.root().join(".local/share/relaxkit/bin/rtk");
    fx.shell.program("rtk", Path::new("/opt/homebrew/bin/rtk"));
    fx.shell.on(
        &format!("{} gain", pack_rtk.display()),
        Output::ok("gain\n"),
    );
    fx.shell.on("rtk init -g", Output::ok(""));
    fx.shell.on("rtk telemetry disable", Output::ok(""));

    let report = fx.run(false, Some("rtk")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines.iter().all(|l| !l.starts_with("brew")),
        "brew was run: {lines:?}"
    );
    assert!(pack_rtk.is_file(), "the pack copy was not written");
    // The pack copy was absent before this run, so the run journals it as created: undo removes it again.
    let records = journal::read(&KitDirs::rooted(fx.root())).unwrap();
    assert!(
        records
            .iter()
            .any(|record| record.file.ends_with("bin/rtk") && record.created),
        "the new pack copy is not journaled as created: {records:?}"
    );
    let rtk = row(&report, "rtk");
    assert!(matches!(rtk.result, RowResult::Updated), "{rtk:?}");
    assert!(
        rtk.note.as_deref().unwrap_or("").contains("brew"),
        "{rtk:?}"
    );
    let lock = fx.read_lock();
    assert_eq!(lock.tools["rtk"].version, "1.2.1");
    assert_eq!(lock.tools["rtk"].route, "script");
    // The configuration steps run again after the binary changed (K65 with the rtk recipe).
    assert!(lines.iter().any(|l| l == "rtk init -g"), "{lines:?}");
}

#[test]
fn dangling_pack_rtk_link_is_removed_before_the_write() {
    // A dangling link at the pack rtk path (K65, batch 10 requirement 3). The release must be written as a regular
    // file, and nothing may be created where the link points.
    let fx = Fixture::new();
    fx.lock(&[("rtk", "1.1.0", "script", &["claude"])]);
    serve_rtk(&fx);
    let pack_rtk = fx.root().join(".local/share/relaxkit/bin/rtk");
    let elsewhere = fx.root().join("elsewhere/rtk");
    fs::create_dir_all(pack_rtk.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &pack_rtk).unwrap();
    fx.shell.program("rtk", Path::new("/opt/homebrew/bin/rtk"));
    fx.shell.on(
        &format!("{} gain", pack_rtk.display()),
        Output::ok("gain\n"),
    );
    fx.shell.on("rtk init -g", Output::ok(""));
    fx.shell.on("rtk telemetry disable", Output::ok(""));

    let report = fx.run(false, Some("rtk")).unwrap();

    assert!(
        matches!(row(&report, "rtk").result, RowResult::Updated),
        "{:?}",
        row(&report, "rtk")
    );
    assert!(!elsewhere.exists(), "the write went through the link");
    let meta = fs::symlink_metadata(&pack_rtk).unwrap();
    assert!(meta.file_type().is_file(), "the pack copy is still a link");
}

#[test]
fn nvm_and_uv_are_pinned_and_never_run() {
    let fx = Fixture::new();
    fx.lock(&[
        ("nvm", "v0.41.0", "script", &["claude"]),
        ("uv", "0.13.9", "script", &["claude"]),
    ]);

    let report = fx.run(false, None).unwrap();

    for tool in ["nvm", "uv"] {
        assert_eq!(
            row(&report, tool).result,
            RowResult::NotTouched("pinned (excluded from latest)".to_string()),
            "{tool}"
        );
    }
    let lines = fx.shell.lines();
    assert!(
        lines.iter().all(|l| !l.contains("self update")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|l| !l.contains("install.sh")), "{lines:?}");
}

#[test]
fn equal_versions_are_current_with_no_mutating_call() {
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.87", "npm", &["claude"])]);
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );

    let report = fx.run(false, Some("playwright")).unwrap();

    assert_eq!(row(&report, "playwright").result, RowResult::Current);
    assert!(fx.shell.cmds().is_empty(), "{:?}", fx.shell.lines());
    assert!(report.restart.is_empty());
}

#[test]
fn a_partial_update_keeps_the_other_hosts_version() {
    // Superpowers is on both hosts at 7.0.2, but only Claude Code is installed now, so only its version moves.
    let fx = Fixture::new();
    fx.lock(&[(
        "superpowers",
        "7.0.2",
        "claude-plugin:anthropic-plugin-directory",
        &["claude", "opencode"],
    )]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    Registry::new(&[("superpowers", "7.0.2")], &[("superpowers", "7.0.3")]).attach(&fx.shell);
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));

    let report = fx.run(false, Some("superpowers")).unwrap();

    assert!(matches!(
        row(&report, "superpowers").result,
        RowResult::Updated
    ));
    let entry = fx.read_lock().tools["superpowers"].clone();
    assert_eq!(entry.version, "7.0.3");
    assert_eq!(entry.version_for("claude"), "7.0.3");
    assert_eq!(entry.version_for("opencode"), "7.0.2");
    assert_eq!(
        entry.hosts,
        vec!["claude".to_string(), "opencode".to_string()]
    );
}

#[test]
fn an_unpinned_claude_plugin_moves_only_when_the_host_reports_a_new_version() {
    let fx = Fixture::new();
    fx.lock(&[(
        "claude-md-management",
        "1.0.0",
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);
    let reported = Arc::new(Mutex::new("1.0.0".to_string()));
    let read = reported.clone();
    fx.shell.answer(
        "claude plugin list",
        Arc::new(move |_| {
            Output::ok(&format!(
                "claude-md-management@anthropic-plugin-directory  Version: {}\n",
                read.lock().unwrap()
            ))
        }),
    );
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell
        .on("claude plugin update claude-md-management@", Output::ok(""));
    let bump = reported.clone();
    fx.shell.effect(
        "claude plugin update claude-md-management@",
        Arc::new(move |_| *bump.lock().unwrap() = "1.1.0".to_string()),
    );

    let report = fx.run(false, Some("claude-md-management")).unwrap();

    assert!(matches!(
        row(&report, "claude-md-management").result,
        RowResult::Updated
    ));
    assert_eq!(
        fx.read_lock().tools["claude-md-management"].version,
        "1.1.0"
    );
    assert!(fx.shell.lines().contains(
        &"claude plugin update claude-md-management@anthropic-plugin-directory".to_string()
    ));
}

/// A Claude Code plugin registry as the fake host sees it. `claude plugin list` shows each plugin at its installed
/// version, and `claude plugin update <name>@<marketplace>` moves the plugin to the version its marketplace serves.
#[derive(Clone)]
struct Registry {
    installed: Arc<Mutex<BTreeMap<String, String>>>,
    served: BTreeMap<String, String>,
}

impl Registry {
    fn new(installed: &[(&str, &str)], served: &[(&str, &str)]) -> Registry {
        Registry {
            installed: Arc::new(Mutex::new(
                installed
                    .iter()
                    .map(|(name, version)| (name.to_string(), version.to_string()))
                    .collect(),
            )),
            served: served
                .iter()
                .map(|(name, version)| (name.to_string(), version.to_string()))
                .collect(),
        }
    }

    /// Wires the registry into a shell: the listing answers, and each plugin update moves the plugin.
    fn attach(&self, shell: &Shell) {
        let listed = self.clone();
        shell.answer(
            "claude plugin list",
            Arc::new(move |_| {
                let text: String = listed
                    .installed
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(name, version)| {
                        let marketplace = if name == "caveman" {
                            "caveman"
                        } else {
                            "anthropic-plugin-directory"
                        };
                        format!("{name}@{marketplace}  Version: {version}\n")
                    })
                    .collect();
                Output::ok(&text)
            }),
        );
        let moved = self.clone();
        shell.effect(
            "claude plugin update ",
            Arc::new(move |cmd| {
                let spec = cmd.argv().last().cloned().unwrap_or_default();
                let name = spec.split('@').next().unwrap_or_default().to_string();
                if let Some(version) = moved.served.get(&name) {
                    moved
                        .installed
                        .lock()
                        .unwrap()
                        .insert(name, version.clone());
                }
            }),
        );
    }
}

/// Serves the registry entries that tell the update which runtime the candidate 0.7.1 declares: the plugin's own
/// entry with its runtime dependency `range`, and the runtime's published versions.
fn serve_sandbox_candidate(fx: &Fixture, range: &str, published: &[&str]) {
    fx.serve(
        "https://registry.npmjs.org/opencode-sandbox",
        &format!(
            r#"{{"dist-tags":{{"latest":"0.7.1"}},"versions":{{"0.7.1":{{"dependencies":{{"@anthropic-ai/sandbox-runtime":"{range}"}}}}}}}}"#
        ),
    );
    let versions: Vec<String> = published.iter().map(|v| format!("\"{v}\":{{}}")).collect();
    fx.serve(
        "https://registry.npmjs.org/@anthropic-ai%2fsandbox-runtime",
        &format!(r#"{{"versions":{{{}}}}}"#, versions.join(",")),
    );
}

/// A fixture whose only host is OpenCode, with a locked opencode-sandbox 0.7.0. The candidate 0.7.1 declares the
/// runtime `runtime` (its self-test runs that exact version), and the runtime's published versions are `published`.
fn sandbox_fixture(runtime: &str, published: &[&str], self_test: &'static str) -> Fixture {
    sandbox_fixture_with(runtime, published, self_test, true)
}

fn sandbox_fixture_with(
    runtime: &str,
    published: &[&str],
    self_test: &'static str,
    serve_candidate: bool,
) -> Fixture {
    let mut fx = Fixture::new();
    let config = fx.root().join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "{\"plugins\": [\"opencode-sandbox@0.7.0\"]}\n").unwrap();
    fx.hosts = vec![Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        fx.root().join(".config/opencode"),
    ))];
    fx.lock(&[(
        "opencode-sandbox",
        "0.7.0",
        "opencode-plugin",
        &["opencode"],
    )]);
    fx.serve(
        "https://registry.npmjs.org/opencode-sandbox/latest",
        r#"{"version":"0.7.1"}"#,
    );
    if serve_candidate {
        serve_sandbox_candidate(&fx, &format!("^{runtime}"), published);
    }
    let npx = fx.npx();
    fx.shell.on(
        &format!("{npx} --yes @anthropic-ai/sandbox-runtime@{runtime}"),
        Output::ok(self_test),
    );
    fx.shell.on("opencode plugin remove", Output::ok(""));
    fx.shell.on("opencode plugin add", Output::ok(""));
    fx.shell.on("opencode --version", Output::ok("2.2.6\n"));
    fx
}

#[test]
fn sandbox_selftest_runs_the_runtime_the_candidate_declares() {
    // The candidate 0.7.1 declares runtime 0.0.83, so its self-test runs 0.0.83 and not the pin 0.0.76 (K83). A
    // failing probe keeps the locked plugin.
    let fx = sandbox_fixture("0.0.83", &["0.0.76", "0.0.83"], "status=0\n");

    let report = fx.run(false, Some("opencode-sandbox")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines.iter().any(|l| l.contains("sandbox-runtime@0.0.83")),
        "the candidate's runtime was not self-tested: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("sandbox-runtime@0.0.76")),
        "the pinned runtime was used for the candidate: {lines:?}"
    );
    assert_eq!(report.failed, vec!["opencode-sandbox".to_string()]);
    assert_eq!(fx.read_lock().tools["opencode-sandbox"].version, "0.7.0");
}

#[test]
fn sandbox_plugin_stays_when_the_candidate_metadata_cannot_be_read() {
    // A passing self-test is not enough: without the candidate's runtime nothing can be proven, so nothing moves.
    let fx = sandbox_fixture_with("0.0.76", &["0.0.76"], "403\nstatus=7\n", false);

    let report = fx.run(false, Some("opencode-sandbox")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("opencode plugin add opencode-sandbox@0.7.1")),
        "the plugin moved without its metadata: {lines:?}"
    );
    assert_eq!(report.failed, vec!["opencode-sandbox".to_string()]);
    assert_eq!(fx.read_lock().tools["opencode-sandbox"].version, "0.7.0");
}

#[test]
fn undo_keeps_an_existing_sandbox_config_when_the_plugin_does_not_move() {
    let fx = sandbox_fixture("0.0.76", &["0.0.76"], "status=0\n");
    let config = fx.root().join(".config/opencode/sandbox.json");
    fs::write(&config, "{\"mode\": \"advisory\"}\n").unwrap();

    let report = fx.run(false, Some("opencode-sandbox")).unwrap();
    assert_eq!(report.failed, vec!["opencode-sandbox".to_string()]);
    undo(&fx.ctx(), Some(&fx.run)).unwrap();

    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "{\"mode\": \"advisory\"}\n",
        "undo changed a sandbox config the update did not touch"
    );
}

#[test]
fn undo_does_not_delete_a_sandbox_config_the_update_never_created() {
    let fx = sandbox_fixture("0.0.76", &["0.0.76"], "status=0\n");
    let config = fx.root().join(".config/opencode/sandbox.json");
    assert!(!config.exists());

    let report = fx.run(false, Some("opencode-sandbox")).unwrap();
    assert_eq!(report.failed, vec!["opencode-sandbox".to_string()]);
    // The user writes their own config after the update and before undo.
    fs::write(&config, "{\"mode\": \"enforce\"}\n").unwrap();
    undo(&fx.ctx(), Some(&fx.run)).unwrap();

    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "{\"mode\": \"enforce\"}\n",
        "undo deleted a file the update never created"
    );
}

/// The text a binary run printed on stdout.
fn stdout_of(scratch: &Scratch, args: &[&str]) -> String {
    let output = scratch
        .relaxkit()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn check_through_the_binary_exits_zero_offline_and_runs_no_mutating_command() {
    // With no network (the proxy is closed), a check still succeeds: a registry failure is "newest unknown", not a
    // failure of the run (K70). The stubs must see no mutating call and the home must not change.
    let scratch = Scratch::new();
    let before = tree(scratch.home());

    let text = stdout_of(&scratch, &["update", "--check"]);

    assert_eq!(tree(scratch.home()), before, "a check changed a file");
    assert!(
        scratch.calls().iter().all(|call| !mutating(call)),
        "a check ran a mutating command: {:?}",
        scratch.calls()
    );
    assert!(
        text.contains("relaxkit itself: updated in a later step"),
        "{text}"
    );
    assert!(text.contains("not touched"), "{text}");
}

#[test]
fn flags_select_scope_at_the_command_line() {
    let scratch = Scratch::new();

    // A plain update names the self step, which this build does not run yet. `--tools` leaves it out (K63).
    let plain = stdout_of(&scratch, &["update", "--check"]);
    let tools_only = stdout_of(&scratch, &["update", "--check", "--tools"]);
    assert!(plain.contains("relaxkit itself"), "{plain}");
    assert!(!tools_only.contains("relaxkit itself"), "{tools_only}");

    // A tool name runs that row only.
    let one = stdout_of(&scratch, &["update", "--check", "rtk"]);
    assert!(one.contains("rtk"), "{one}");
    assert!(!one.contains("playwright"), "{one}");

    // A name that is not in the pack is an error, and nothing runs.
    scratch
        .relaxkit()
        .args(["update", "--check", "no-such-tool"])
        .assert()
        .failure();
}

/// Serves the latest rtk release (v1.2.1) for this platform: its checksums, its archive, and a `tar` that unpacks an rtk.
fn serve_rtk(fx: &Fixture) {
    fx.serve(
        "https://api.github.com/repos/rtk-ai/rtk/releases/latest",
        r#"{"tag_name":"v1.2.1"}"#,
    );
    let archive = match (current_os().unwrap(), current_arch().unwrap()) {
        (Os::MacOs, Arch::Aarch64) => "rtk-aarch64-apple-darwin.tar.gz",
        (Os::MacOs, Arch::X86_64) => "rtk-x86_64-apple-darwin.tar.gz",
        (Os::Linux, Arch::X86_64) => "rtk-x86_64-unknown-linux-musl.tar.gz",
        (Os::Linux, Arch::Aarch64) => "rtk-aarch64-unknown-linux-musl.tar.gz",
    };
    let body = b"fake archive".to_vec();
    let digest: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let base = "https://github.com/rtk-ai/rtk/releases/download/v1.2.1";
    fx.net.clone().with(
        &format!("{base}/checksums.txt"),
        &format!("{digest}  {archive}\n"),
    );
    fx.net
        .clone()
        .with_bytes(&format!("{base}/{archive}"), body);
    // `tar -xf <archive> -C <dir>` unpacks an rtk binary into the extraction directory, as the release archive does.
    fx.shell.effect(
        "tar -xf",
        Arc::new(|cmd| {
            let args = cmd.argv();
            let dir = PathBuf::from(&args[args.iter().position(|a| a == "-C").unwrap() + 1]);
            fs::write(dir.join("rtk"), "#!/bin/sh\necho rtk 1.2.1\n").unwrap();
        }),
    );
    fx.shell.on("tar -xf", Output::ok(""));
}
