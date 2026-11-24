//! `relaxkit update` for the tools. Every test runs against a temporary root, a scripted
//! runner that records each command with its environment, and FakeNet. No test touches the real home, the real
//! network, or a real host command.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use relaxkit::commands::status::{Note, ToolRow, ToolState};
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
            group: None,
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
    // The check reads the Claude host's listing: superpowers is installed at the version the lock records.
    fx.shell.on(
        "claude plugin list",
        Output::ok(
            "Installed plugins:\n\n  \u{276f} superpowers@anthropic-plugin-directory\n    Version: 7.0.2-22f0b463e7a5\n    Scope: user\n    Status: \u{2714} enabled\n",
        ),
    );
    // No runner answer and no served registry for rtk: its row is "newest unknown", not a failure of the run.
    let before = tree(fx.root());

    let report = fx.run(true, None).unwrap();

    // A check reads each host's plugin list or version, as status does, and runs nothing that changes it.
    let changing: Vec<String> = fx
        .shell
        .lines()
        .into_iter()
        .filter(|line| {
            !["list", "--version", "status"]
                .iter()
                .any(|word| line.contains(word))
        })
        .collect();
    assert!(changing.is_empty(), "a check ran {changing:?}");
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
fn a_tool_whose_hosts_differ_shows_each_hosts_own_version_as_installed() {
    let fx = Fixture::new();
    let tools = BTreeMap::from([(
        "superpowers".to_string(),
        LockedTool {
            version: "7.0.2-22f0b463e7a5".to_string(),
            route: "claude-plugin:anthropic-plugin-directory".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([
                ("claude".to_string(), "7.0.2-22f0b463e7a5".to_string()),
                ("opencode".to_string(), "7.0.2".to_string()),
            ]),
        },
    )]);
    let lock = Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    };
    lock.save(&fx.ctx(), &RunId::new()).unwrap();

    let report = fx.run(true, Some("superpowers")).unwrap();

    assert_eq!(
        row(&report, "superpowers").installed.as_deref(),
        Some("claude 7.0.2-22f0b463e7a5, opencode 7.0.2")
    );
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
    // Claude Code plugins: marketplace first, then the plugin.
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
    // npm pins are rewritten through the MCP remove and add, at the exact new version.
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
    // Serena reinstalls at the newest pin with --force, then runs `serena init`.
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
fn a_host_behind_the_locked_version_is_not_called_current() {
    // The lock says caveman 4.2.0 on OpenCode, but the CLI relaxkit installed there is 4.1.0. The check must read what
    // is installed (as status does), so the row is available, not current.
    let mut fx = Fixture::new();
    let config = fx.root().join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "{\"plugins\": [\"./plugins/caveman\"]}\n").unwrap();
    let cli = fx
        .root()
        .join(".local/share/relaxkit/node/lib/node_modules/@caveman-ai/cli");
    fs::create_dir_all(&cli).unwrap();
    fs::write(
        cli.join("package.json"),
        "{\"name\": \"@caveman-ai/cli\", \"version\": \"4.1.0\"}",
    )
    .unwrap();
    fx.hosts = vec![Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        fx.root().join(".config/opencode"),
    ))];
    fx.lock(&[("caveman", "4.2.0", "npm", &["opencode"])]);
    fx.serve(
        "https://registry.npmjs.org/@caveman-ai%2fcli/latest",
        r#"{"version":"4.2.0"}"#,
    );

    let report = fx.run(true, Some("caveman")).unwrap();

    let caveman = row(&report, "caveman");
    assert_eq!(caveman.installed.as_deref(), Some("4.1.0"));
    assert_eq!(caveman.newest.as_deref(), Some("4.2.0"));
    assert_eq!(caveman.result, RowResult::Available);
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
fn tool_not_installed_by_the_pack_is_reported_not_touched() {
    // gh from Homebrew: the pack wrote no lock entry for it, so nothing runs.
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
    // The lock records Playwright 0.0.87 on this host. A repair puts back what the lock says, not the
    // newest release, and the lock itself is not rewritten by a repair.
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.87", "npm", &["claude"])]);
    let before = fx.read_lock();
    fx.shell.on("claude mcp remove", Output::ok(""));
    fx.shell.on("claude mcp add", Output::ok(""));
    let recipes = relaxkit::recipe::load_all().unwrap();
    let playwright = recipes.iter().find(|r| r.id == "playwright").unwrap();
    assert_eq!(playwright.pin, None, "a recipe file names no pin");
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
fn a_repair_of_an_unversioned_cli_installs_the_newest_release_not_the_placeholder() {
    // The lock records caveman as unversioned and its CLI is missing, so the repair installs the CLI. It must use the
    // recipe's pin (4.1.0), never the placeholder word `unversioned`.
    let fx = Fixture::new();
    fx.lock(&[("caveman", "unversioned", "npm", &["claude"])]);
    Registry::new(&[("caveman", "4.1.0")], &[]).attach(&fx.shell);
    fx.shell.on(
        "claude plugin marketplace list",
        Output::ok("  \u{276f} caveman  (github JuliusBrussee/caveman)\n"),
    );
    let npm = fx.npm();
    fx.shell.on(
        &format!("{npm} install -g @caveman-ai/cli@4.1.0"),
        Output::ok(""),
    );
    // Nothing exact is locked for the CLI, so the repair takes the newest release from the registry.
    fx.serve(
        "https://registry.npmjs.org/@caveman-ai%2fcli/latest",
        r#"{"version":"4.1.0"}"#,
    );
    let recipes = relaxkit::recipe::load_all().unwrap();
    let caveman = recipes.iter().find(|r| r.id == "caveman").unwrap();
    let ctx = fx.ctx();
    let node = relaxkit::runtime::nvm::current_node(&ctx).unwrap();
    let engine = Engine {
        ctx: &ctx,
        run: &ctx.run,
        hosts: &fx.hosts,
        recipes: &recipes,
        node: &node,
    };

    let _ = engine.reinstall(caveman, HostKind::Claude);

    let lines = fx.shell.lines();
    assert!(
        lines.iter().all(|l| !l.contains("unversioned")),
        "the placeholder reached a command: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("@caveman-ai/cli@4.1.0")),
        "the CLI was not installed at the newest release: {lines:?}"
    );
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
    // not confirmed, but Claude's plugin changed and must be listed to restart.
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
    // lock must not move, the tool goes back to its locked spec, and the report names it.
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
fn status_reports_no_drift_after_a_partial_update() {
    // Claude moved to 7.0.3 and OpenCode stayed at 7.0.2. The lock records each host's own version, so status compares
    // each host with it and reports no drift on either.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    let config = root.join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        "{\"plugins\": [\"superpowers@git+https://github.com/obra/superpowers.git#v7.0.2\"]}\n",
    )
    .unwrap();
    let claude_info = HostInfo {
        kind: HostKind::Claude,
        binary: PathBuf::from("/usr/local/bin/claude"),
        version: "2.1.330".to_string(),
    };
    let opencode_info = HostInfo {
        kind: HostKind::Opencode,
        binary: PathBuf::from("/usr/local/bin/opencode"),
        version: "2.2.6".to_string(),
    };
    fx.hosts = vec![
        Box::new(ClaudeHost::new(
            claude_info,
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        Box::new(OpencodeHost::new(
            opencode_info,
            root.join(".config/opencode"),
        )),
    ];
    fx.shell.on(
        "claude plugin list",
        Output::ok("superpowers@anthropic-plugin-directory  Version: 7.0.3\n"),
    );
    fx.lock(&[(
        "superpowers",
        "7.0.2",
        "claude-plugin:anthropic-plugin-directory",
        &["claude", "opencode"],
    )]);
    let before = fx.read_lock().tools["superpowers"].clone();
    let moved = before.moved(
        &["claude".to_string()],
        "7.0.3",
        "claude-plugin:anthropic-plugin-directory",
    );
    let mut tools = BTreeMap::new();
    tools.insert("superpowers".to_string(), moved);
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();

    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();

    let rows: Vec<&ToolRow> = report
        .tools
        .iter()
        .filter(|row| row.id == "superpowers")
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().all(|row| matches!(
            row.state,
            ToolState::AsLocked | ToolState::AsLockedUnrecorded
        )),
        "{rows:?}"
    );
}

#[test]
fn a_claude_update_with_no_reported_version_still_restarts_claude() {
    // The plugin is updated on disk and its probe passes, but the listing names no exact version, so the new version
    // cannot be recorded. The row is named as failed, and Claude is still listed to restart.
    let fx = Fixture::new();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.on(
        "claude plugin list",
        Output::ok("superpowers@anthropic-plugin-directory\n"),
    );
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

    assert!(
        matches!(
            row(&report, "superpowers").result,
            RowResult::RollbackFailed(_)
        ),
        "{:?}",
        row(&report, "superpowers")
    );
    assert!(
        report.restart.contains(&HostKind::Claude),
        "a changed Claude plugin was not listed to restart: {report:?}"
    );
}

#[test]
fn rtk_is_not_touched_while_a_host_is_below_its_minimum() {
    // Claude (2.1.300) is below rtk's minimum (2.1.330). The pack binary is shared, so the update
    // does not move it and writes no lock entry. The row is NotTouched and names the host, its version and the minimum.
    // Two runs must read the same, and OpenCode, already at the newest 1.2.1, changes nothing either.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    fx.hosts = vec![
        Box::new(ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.300".to_string(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        Box::new(OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            root.join(".config/opencode"),
        )),
    ];
    let mut tools = BTreeMap::new();
    tools.insert(
        "rtk".to_string(),
        LockedTool {
            version: "1.2.1".to_string(),
            route: "script".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([("claude".to_string(), "1.1.0".to_string())]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    let lock_path = KitDirs::rooted(&root).config.join("lock.toml");
    let before = fs::read_to_string(&lock_path).unwrap();
    serve_rtk(&fx);
    let pack_rtk = root.join(".local/share/relaxkit/bin/rtk");
    fx.shell.on(
        &format!("{} gain", pack_rtk.display()),
        Output::ok("gain\n"),
    );
    fx.shell.on("rtk init -g --opencode", Output::ok(""));
    fx.shell.on("rtk telemetry disable", Output::ok(""));

    for run in 0..2 {
        let report = fx.run(false, Some("rtk")).unwrap();
        let rtk = row(&report, "rtk");
        match &rtk.result {
            RowResult::NotTouched(reason) => assert!(
                reason.contains("Claude Code")
                    && reason.contains("2.1.300")
                    && reason.contains("2.1.330"),
                "run {run}: the reason does not name the host, its version and the minimum: {reason}"
            ),
            other => panic!("run {run}: expected NotTouched, got {other:?}"),
        }
        assert!(report.restart.is_empty(), "run {run}: {report:?}");
        assert!(
            !pack_rtk.exists(),
            "run {run}: the shared pack binary was written"
        );
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            before,
            "run {run}: the lock was rewritten"
        );
    }
}

#[test]
fn a_claude_update_in_the_real_listing_shape_succeeds_and_moves_the_lock() {
    // `claude plugin list` prints the version on the line under the name (verified 2026-11-22). The update must read it
    // and succeed: no rollback, the lock moves to the version the listing now shows.
    let fx = Fixture::new();
    let version = Arc::new(Mutex::new("7.0.2-22f0b463e7a5".to_string()));
    let listed = version.clone();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.answer(
        "claude plugin list",
        Arc::new(move |_| {
            Output::ok(&format!(
                "Installed plugins:\n\n  \u{276f} superpowers@anthropic-plugin-directory\n    Version: {}\n    Scope: user\n    Status: \u{2714} enabled\n",
                listed.lock().unwrap()
            ))
        }),
    );
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    let moved = version.clone();
    fx.shell.effect(
        "claude plugin update superpowers@",
        Arc::new(move |_| *moved.lock().unwrap() = "7.0.3-9a1c2d3e4f50".to_string()),
    );
    fx.lock(&[(
        "superpowers",
        "7.0.2-22f0b463e7a5",
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    assert!(
        matches!(row(&report, "superpowers").result, RowResult::Updated),
        "{:?}",
        row(&report, "superpowers")
    );
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(report.restart.contains(&HostKind::Claude), "{report:?}");
    assert_eq!(
        fx.read_lock().tools["superpowers"].version,
        "7.0.3-9a1c2d3e4f50"
    );
}

#[test]
fn an_opencode_config_with_two_entries_for_the_plugin_is_rolled_back() {
    // `opencode plugin remove` leaves the old spec in place, so after the add the config lists both the old and the new
    // spec. Exactly one entry per plugin is the rule, so the update is rolled back and named, and the lock keeps
    // the old version. The rollback removes the new entry and finds the old one again.
    let mut fx = Fixture::new();
    let config = fx.root().join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = "superpowers@git+https://github.com/obra/superpowers.git#v7.0.2";
    let entries = Arc::new(Mutex::new(vec![old.to_string()]));
    let render = |list: &[String]| {
        format!(
            "{{\"plugins\": [{}]}}\n",
            list.iter()
                .map(|entry| format!("\"{entry}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    fs::write(&config, render(&entries.lock().unwrap())).unwrap();
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
    // The remove of the old spec does nothing: the stale entry stays. Any other remove works.
    let kept = entries.clone();
    let file = config.clone();
    fx.shell.effect(
        "opencode plugin remove",
        Arc::new(move |cmd| {
            let spec = cmd.argv().last().cloned().unwrap_or_default();
            if spec != old {
                let mut list = kept.lock().unwrap();
                list.retain(|entry| *entry != spec);
                fs::write(&file, render(&list)).unwrap();
            }
        }),
    );
    let added = entries.clone();
    let file = config.clone();
    fx.shell.effect(
        "opencode plugin add",
        Arc::new(move |cmd| {
            let spec = cmd.argv().last().cloned().unwrap_or_default();
            let mut list = added.lock().unwrap();
            if !list.contains(&spec) {
                list.push(spec);
            }
            fs::write(&file, render(&list)).unwrap();
        }),
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    let row = row(&report, "superpowers");
    assert!(matches!(row.result, RowResult::RolledBack), "{row:?}");
    assert!(
        row.note.as_deref().unwrap_or("").contains("2 entries"),
        "the note does not name the two entries: {row:?}"
    );
    assert_eq!(report.failed, vec!["superpowers".to_string()]);
    assert_eq!(
        fx.read_lock().tools["superpowers"],
        before.tools["superpowers"],
        "the lock moved"
    );
}

#[test]
fn an_unversioned_lock_entry_is_not_drift() {
    // A Claude plugin locked as `unversioned` (its listing reported no version when it was recorded) is not drift:
    // status says its version is not recorded.
    let fx = Fixture::new();
    fx.shell.on(
        "claude plugin list",
        Output::ok("superpowers@anthropic-plugin-directory  Version: 7.0.2\n"),
    );
    fx.lock(&[(
        "superpowers",
        relaxkit::tools::engine::UNVERSIONED,
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);

    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();

    let rows: Vec<&ToolRow> = report
        .tools
        .iter()
        .filter(|row| row.id == "superpowers")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_ne!(rows[0].state, ToolState::Drift, "{rows:?}");
    assert_eq!(rows[0].state, ToolState::AsLockedUnrecorded, "{rows:?}");
}

#[test]
fn a_first_move_from_an_unversioned_entry_records_without_the_updated_label() {
    // The entry was recorded without a version, so the move records the version now. The row must not claim an update
    // of a version it never knew, and Claude is still listed to restart.
    let fx = Fixture::new();
    let version = Arc::new(Mutex::new("7.0.2".to_string()));
    let listed = version.clone();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.answer(
        "claude plugin list",
        Arc::new(move |_| {
            Output::ok(&format!(
                "superpowers@anthropic-plugin-directory  Version: {}\n",
                listed.lock().unwrap()
            ))
        }),
    );
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    let moved = version.clone();
    fx.shell.effect(
        "claude plugin update superpowers@",
        Arc::new(move |_| *moved.lock().unwrap() = "7.0.3".to_string()),
    );
    fx.lock(&[(
        "superpowers",
        relaxkit::tools::engine::UNVERSIONED,
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.3"}"#,
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    let row = row(&report, "superpowers");
    assert!(matches!(row.result, RowResult::Recorded), "{row:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(report.restart.contains(&HostKind::Claude), "{report:?}");
    assert_eq!(fx.read_lock().tools["superpowers"].version, "7.0.3");
}

#[test]
fn a_claude_plugin_at_the_newest_release_is_current_with_no_call() {
    // The listing reports `7.0.2-22f0b463e7a5`; the newest release tag is `7.0.2`. The build suffix is not a different
    // version, so the plugin is current: a check says so, and an update makes no marketplace or plugin call.
    let fx = Fixture::new();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.on(
        "claude plugin list",
        Output::ok("superpowers@anthropic-plugin-directory  Version: 7.0.2-22f0b463e7a5\n"),
    );
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    fx.lock(&[(
        "superpowers",
        "7.0.2-22f0b463e7a5",
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.2"}"#,
    );

    let checked = fx.run(true, Some("superpowers")).unwrap();
    assert!(
        matches!(row(&checked, "superpowers").result, RowResult::Current),
        "check: {:?}",
        row(&checked, "superpowers")
    );

    let updated = fx.run(false, Some("superpowers")).unwrap();
    assert!(
        matches!(row(&updated, "superpowers").result, RowResult::Current),
        "update: {:?}",
        row(&updated, "superpowers")
    );
    let lines = fx.shell.lines();
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with("claude plugin marketplace update")
                && !l.starts_with("claude plugin update")),
        "a current plugin was updated: {lines:?}"
    );
}

#[test]
fn a_blocked_brew_rtk_does_not_claim_it_was_updated() {
    // A brew-route rtk with a host below its minimum is not touched. Nothing was written, so the row's note must
    // not say the pack copy was updated.
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
    fx.lock(&[("rtk", "1.1.0", "brew", &["claude"])]);
    serve_rtk(&fx);

    let report = fx.run(false, Some("rtk")).unwrap();

    let rtk = row(&report, "rtk");
    assert!(matches!(rtk.result, RowResult::NotTouched(_)), "{rtk:?}");
    let note = rtk.note.clone().unwrap_or_default();
    assert!(
        !note.contains("was updated"),
        "the note claims a write: {rtk:?}"
    );
}

#[test]
fn rtk_restarts_every_host_whose_config_ran_not_only_the_moved_ones() {
    // Claude is already at the newest 1.2.1 and OpenCode is stale at 1.1.0. Only OpenCode moves, but both hosts'
    // configuration steps run against the new binary, so both hosts are listed to restart.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    fx.hosts = vec![
        Box::new(ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.340".to_string(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        Box::new(OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            root.join(".config/opencode"),
        )),
    ];
    let mut tools = BTreeMap::new();
    tools.insert(
        "rtk".to_string(),
        LockedTool {
            version: "1.2.1".to_string(),
            route: "script".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([("opencode".to_string(), "1.1.0".to_string())]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    serve_rtk(&fx);
    let pack_rtk = root.join(".local/share/relaxkit/bin/rtk");
    fx.shell.on(
        &format!("{} gain", pack_rtk.display()),
        Output::ok("gain\n"),
    );
    fx.shell.on("rtk init -g", Output::ok(""));
    fx.shell.on("rtk init -g --opencode", Output::ok(""));
    fx.shell.on("rtk telemetry disable", Output::ok(""));

    let report = fx.run(false, Some("rtk")).unwrap();

    let rtk = row(&report, "rtk");
    assert!(matches!(rtk.result, RowResult::Updated), "{rtk:?}");
    assert!(
        report.restart.contains(&HostKind::Claude),
        "the at-target host's configuration ran but it is not listed: {report:?}"
    );
    assert!(report.restart.contains(&HostKind::Opencode), "{report:?}");
    assert_eq!(fx.read_lock().tools["rtk"].version_for("opencode"), "1.2.1");
}

#[test]
fn a_host_already_at_the_newest_version_is_not_touched_when_another_host_is_behind() {
    // Claude is at the newest 7.0.2 (its listing carries a build suffix) and OpenCode is behind at 7.0.1. Only OpenCode
    // runs: no Claude marketplace or plugin call. OpenCode's probe fails once and then passes, so the rollback is a
    // real rollback to 7.0.1.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    let config = root.join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = "superpowers@git+https://github.com/obra/superpowers.git#v7.0.1";
    fs::write(&config, format!("{{\"plugins\": [\"{old}\"]}}\n")).unwrap();
    fx.hosts = vec![
        Box::new(ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.330".to_string(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        Box::new(OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            root.join(".config/opencode"),
        )),
    ];
    fx.shell.on(
        "claude plugin list",
        Output::ok("superpowers@anthropic-plugin-directory  Version: 7.0.2-22f0b463e7a5\n"),
    );
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    let mut tools = BTreeMap::new();
    tools.insert(
        "superpowers".to_string(),
        LockedTool {
            version: "7.0.1".to_string(),
            route: "opencode-plugin".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([(
                "claude".to_string(),
                "7.0.2-22f0b463e7a5".to_string(),
            )]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.2"}"#,
    );
    fx.shell.on("opencode plugin remove", Output::ok(""));
    fx.shell.on("opencode plugin add", Output::ok(""));
    // The first probe fails, the probe after the rollback passes.
    let probes = Arc::new(Mutex::new(0));
    let counted = probes.clone();
    fx.shell.answer(
        "opencode --version",
        Arc::new(move |_| {
            let mut n = counted.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Output::fail(1, "broken")
            } else {
                Output::ok("2.2.6\n")
            }
        }),
    );
    let cfg = config.clone();
    fx.shell.effect(
        "opencode plugin add",
        Arc::new(move |cmd| {
            let spec = cmd.argv().last().cloned().unwrap_or_default();
            fs::write(&cfg, format!("{{\"plugins\": [\"{spec}\"]}}\n")).unwrap();
        }),
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with("claude plugin marketplace update")
                && !l.starts_with("claude plugin update")),
        "the at-target Claude host was touched: {lines:?}"
    );
    let row = row(&report, "superpowers");
    assert!(
        matches!(row.result, RowResult::RolledBack),
        "a probe failure on OpenCode must roll OpenCode back, not fail the rollback: {row:?}"
    );
    assert_eq!(report.failed, vec!["superpowers".to_string()]);
    assert_eq!(report.restart, vec![HostKind::Opencode], "{report:?}");
}

#[test]
fn an_mcp_host_already_at_the_newest_version_keeps_its_entry() {
    // Playwright is at the newest 0.0.87 on Claude and behind on OpenCode (0.0.86). Only OpenCode's MCP entry is
    // removed and added again; Claude's entry is neither removed nor re-added.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    fx.hosts.push(Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        root.join(".config/opencode"),
    )));
    let mut tools = BTreeMap::new();
    tools.insert(
        "playwright".to_string(),
        LockedTool {
            version: "0.0.87".to_string(),
            route: "npm".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([("opencode".to_string(), "0.0.86".to_string())]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    fx.serve(
        "https://registry.npmjs.org/@playwright%2fmcp/latest",
        r#"{"version":"0.0.87"}"#,
    );
    let npx = fx.npx();
    fx.shell.on(
        &format!("{npx} -y @playwright/mcp@0.0.87 install-browser chromium"),
        Output::ok(""),
    );
    fx.shell.on(
        &fx.mcp_probe("@playwright/mcp", "0.0.87"),
        Output::ok("ok\n"),
    );
    fx.shell.on("opencode mcp add", Output::ok(""));

    let report = fx.run(false, Some("playwright")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines.iter().all(|l| !l.starts_with("claude mcp")),
        "the at-target Claude entry was removed or re-added: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("opencode mcp add")),
        "the behind OpenCode entry was not re-added: {lines:?}"
    );
    assert!(
        matches!(row(&report, "playwright").result, RowResult::Updated),
        "{:?}",
        row(&report, "playwright")
    );
    assert_eq!(
        fx.read_lock().tools["playwright"].version_for("opencode"),
        "0.0.87"
    );
    assert!(!report.restart.contains(&HostKind::Claude), "{report:?}");
}

#[test]
fn a_row_with_a_known_move_keeps_the_updated_label() {
    // Claude was recorded without a version (unversioned) and OpenCode is at 7.0.1. Both move to 7.0.2. The
    // unversioned Claude move carries no label of its own, but OpenCode's move from a known version does, so the row
    // says "updated".
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    let config = root.join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = "superpowers@git+https://github.com/obra/superpowers.git#v7.0.1";
    let entries = Arc::new(Mutex::new(vec![old.to_string()]));
    let initial = format!("{{\"plugins\": [\"{old}\"]}}\n");
    fs::write(&config, initial).unwrap();
    fx.hosts = vec![
        Box::new(ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.330".to_string(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        Box::new(OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            root.join(".config/opencode"),
        )),
    ];
    let version = Arc::new(Mutex::new("7.0.1".to_string()));
    let listed = version.clone();
    fx.shell
        .on("claude plugin marketplace update", Output::ok(""));
    fx.shell.answer(
        "claude plugin list",
        Arc::new(move |_| {
            Output::ok(&format!(
                "superpowers@anthropic-plugin-directory  Version: {}\n",
                listed.lock().unwrap()
            ))
        }),
    );
    fx.shell
        .on("claude plugin update superpowers@", Output::ok(""));
    let moved = version.clone();
    fx.shell.effect(
        "claude plugin update superpowers@",
        Arc::new(move |_| *moved.lock().unwrap() = "7.0.2".to_string()),
    );
    fx.shell.on("opencode plugin remove", Output::ok(""));
    fx.shell.on("opencode plugin add", Output::ok(""));
    fx.shell.on("opencode --version", Output::ok("2.2.6\n"));
    let cfg = config.clone();
    let added = entries.clone();
    fx.shell.effect(
        "opencode plugin add",
        Arc::new(move |cmd| {
            let spec = cmd.argv().last().cloned().unwrap_or_default();
            let mut list = added.lock().unwrap();
            list.clear();
            list.push(spec);
            fs::write(&cfg, format!("{{\"plugins\": [\"{}\"]}}\n", list[0])).unwrap();
        }),
    );
    let mut tools = BTreeMap::new();
    tools.insert(
        "superpowers".to_string(),
        LockedTool {
            version: "unversioned".to_string(),
            route: "opencode-plugin".to_string(),
            hosts: vec!["claude".to_string(), "opencode".to_string()],
            host_versions: BTreeMap::from([("opencode".to_string(), "7.0.1".to_string())]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    fx.serve(
        "https://api.github.com/repos/obra/superpowers/releases/latest",
        r#"{"tag_name":"v7.0.2"}"#,
    );

    let report = fx.run(false, Some("superpowers")).unwrap();

    let row = row(&report, "superpowers");
    assert!(matches!(row.result, RowResult::Updated), "{row:?}");
    assert!(report.failed.is_empty(), "{report:?}");
}

#[test]
fn an_unversioned_entry_for_a_missing_tool_reports_not_found() {
    // The lock says unversioned, but the plugin is not in the listing: status must say it was not found, not that it
    // is as locked.
    let fx = Fixture::new();
    fx.shell.on("claude plugin list", Output::ok(""));
    fx.lock(&[(
        "superpowers",
        relaxkit::tools::engine::UNVERSIONED,
        "claude-plugin:anthropic-plugin-directory",
        &["claude"],
    )]);

    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();

    let rows: Vec<&ToolRow> = report
        .tools
        .iter()
        .filter(|row| row.id == "superpowers")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].state, ToolState::NotFound, "{rows:?}");
    assert_eq!(rows[0].installed, None, "{rows:?}");
}

#[test]
fn a_rollback_never_pins_an_unversioned_locked_version() {
    // Caveman is locked `unversioned` on OpenCode. Its update fails (the GitHub fetch has no stub here, so the failure
    // is before the probe). The rollback must not call the install with `unversioned` as a pin: it returns
    // RollbackFailed, names the tool, and leaves the lock byte-identical.
    let mut fx = Fixture::new();
    let root = fx.root().to_path_buf();
    fx.hosts.push(Box::new(OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        },
        root.join(".config/opencode"),
    )));
    let mut tools = BTreeMap::new();
    tools.insert(
        "caveman".to_string(),
        LockedTool {
            version: "unversioned".to_string(),
            route: "npm".to_string(),
            hosts: vec!["opencode".to_string()],
            host_versions: BTreeMap::from([("opencode".to_string(), "unversioned".to_string())]),
        },
    );
    Lockfile {
        relaxkit: "0.1.0".to_string(),
        tools,
    }
    .save(&fx.ctx(), &fx.run)
    .unwrap();
    let lock_path = KitDirs::rooted(&root).config.join("lock.toml");
    let before = fs::read(&lock_path).unwrap();
    fx.serve(
        "https://registry.npmjs.org/@caveman-ai%2fcli/latest",
        r#"{"version":"4.2.0"}"#,
    );
    let npm = fx.npm();
    fx.shell.on(
        &format!("{npm} install -g @caveman-ai/cli@4.2.0"),
        Output::ok(""),
    );

    let report = fx.run(false, Some("caveman")).unwrap();

    let lines = fx.shell.lines();
    assert!(
        lines.iter().all(|l| !l.contains("unversioned")),
        "the rollback pinned the unversioned placeholder: {lines:?}"
    );
    let row = row(&report, "caveman");
    assert_eq!(row.tool, "caveman", "{row:?}");
    assert!(
        matches!(&row.result, RowResult::RollbackFailed(reason) if reason.contains("not recorded")),
        "{row:?}"
    );
    assert_eq!(fs::read(&lock_path).unwrap(), before, "the lock changed");
}

#[test]
fn an_mcp_entry_is_shown_as_not_read_not_as_missing() {
    // Playwright is locked at 0.0.86 on Claude. Status cannot read an MCP entry, so the line says so: no "not found",
    // and no drift line.
    let fx = Fixture::new();
    fx.lock(&[("playwright", "0.0.86", "npm", &["claude"])]);

    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();

    let rows: Vec<&ToolRow> = report
        .tools
        .iter()
        .filter(|row| row.id == "playwright")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].state, ToolState::McpNotRead, "{rows:?}");
    assert_eq!(rows[0].locked, "0.0.86", "{rows:?}");
}

#[test]
fn status_reports_a_missing_gh_copy_and_not_a_present_one() {
    // gh is not a recipe: status does not print the "no recipe carries it" line for its entry. Its one line
    // is shown only when the pack copy is missing, which names gh and the path.
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.9", "release", &[])]);

    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();
    let gh_notes: Vec<&Note> = report
        .notes
        .iter()
        .filter(|note| note.text.starts_with("gh:"))
        .collect();
    assert_eq!(gh_notes.len(), 1, "{:?}", report.notes);
    assert!(
        gh_notes[0].text.contains("pack copy missing"),
        "{gh_notes:?}"
    );

    pack_gh(&fx);
    let report = relaxkit::commands::status::report_for(&fx.ctx(), &fx.hosts).unwrap();
    assert!(
        report
            .notes
            .iter()
            .all(|note| !note.text.starts_with("gh:")),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_locked_gh_with_no_pack_copy_is_installed_again_and_is_not_current() {
    // The lock says 9.9.9 and the newest release is 9.9.9, but the pack copy is gone: the update installs it again
    // rather than reporting Current.
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.9", "release", &[])]);
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    let bin = KitDirs::rooted(fx.root()).bin.join("gh");
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );

    let report = fx.run(false, Some("gh")).unwrap();

    assert!(
        !matches!(row(&report, "gh").result, RowResult::Current),
        "{:?}",
        row(&report, "gh")
    );
    assert_eq!(
        fs::read_to_string(&bin).unwrap(),
        "#!/bin/sh\necho gh version 9.9.9\n"
    );
    assert!(report.failed.is_empty(), "{report:?}");
}

/// The gh release archive for this platform, named as the release publishes it.
fn gh_archive(version: &str) -> String {
    match (current_os().unwrap(), current_arch().unwrap()) {
        (Os::MacOs, Arch::Aarch64) => format!("gh_{version}_macOS_arm64.zip"),
        (Os::MacOs, Arch::X86_64) => format!("gh_{version}_macOS_amd64.zip"),
        (Os::Linux, Arch::X86_64) => format!("gh_{version}_linux_amd64.tar.gz"),
        (Os::Linux, Arch::Aarch64) => format!("gh_{version}_linux_arm64.tar.gz"),
    }
}

/// Serves gh's newest release: its tag, the archive (with `digest` as its checksums line, or the real digest when
/// none is given), and a `tar -xf` that unpacks `binary` as the release's `bin/gh`.
fn serve_gh(fx: &Fixture, version: &str, archive: &[u8], digest: Option<String>, binary: &str) {
    fx.serve(
        "https://api.github.com/repos/cli/cli/releases/latest",
        &format!(r#"{{"tag_name":"v{version}"}}"#),
    );
    let name = gh_archive(version);
    let base = format!("https://github.com/cli/cli/releases/download/v{version}");
    let digest = digest.unwrap_or_else(|| {
        use sha2::{Digest, Sha256};
        Sha256::digest(archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    });
    fx.net.clone().with(
        &format!("{base}/gh_{version}_checksums.txt"),
        &format!("{digest}  {name}\n"),
    );
    fx.net
        .clone()
        .with_bytes(&format!("{base}/{name}"), archive.to_vec());
    let stem = name
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".zip")
        .to_string();
    let binary = binary.to_string();
    fx.shell.effect(
        "tar -xf",
        Arc::new(move |cmd| {
            let args = cmd.argv();
            let dir = PathBuf::from(&args[args.iter().position(|a| a == "-C").unwrap() + 1]);
            let bin_dir = dir.join(&stem).join("bin");
            fs::create_dir_all(&bin_dir).unwrap();
            fs::write(bin_dir.join("gh"), &binary).unwrap();
        }),
    );
    fx.shell.on("tar -xf", Output::ok(""));
}

/// The pack copy of gh, as it sits in the pack bin directory before an update.
fn pack_gh(fx: &Fixture) -> PathBuf {
    let bin = KitDirs::rooted(fx.root()).bin;
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\necho gh version 9.9.8\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

#[test]
fn gh_at_the_newest_release_is_current_with_no_download() {
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.9", "release", &[])]);
    pack_gh(&fx);
    serve_gh(&fx, "9.9.9", b"archive", None, "");

    let report = fx.run(false, Some("gh")).unwrap();

    assert!(
        matches!(row(&report, "gh").result, RowResult::Current),
        "{:?}",
        row(&report, "gh")
    );
    assert!(
        fx.shell.lines().iter().all(|line| !line.starts_with("tar")),
        "{:?}",
        fx.shell.lines()
    );
    assert!(report.failed.is_empty());
}

#[test]
fn gh_is_moved_to_the_newest_release_and_recorded_with_the_archive_for_this_platform() {
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    let bin = pack_gh(&fx);
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );

    let report = fx.run(false, Some("gh")).unwrap();

    let archive = gh_archive("9.9.9");
    let lines = fx.shell.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("tar -xf") && line.contains(&archive)),
        "the archive for this platform was not unpacked: {lines:?}"
    );
    assert!(
        matches!(row(&report, "gh").result, RowResult::Updated),
        "{:?}",
        row(&report, "gh")
    );
    assert_eq!(
        fs::read_to_string(&bin).unwrap(),
        "#!/bin/sh\necho gh version 9.9.9\n"
    );
    let entry = &fx.read_lock().tools["gh"];
    assert_eq!(entry.version, "9.9.9");
    assert_eq!(entry.route, "release");
    assert!(report.failed.is_empty());
}

#[test]
fn a_gh_checksum_mismatch_writes_nothing_and_keeps_the_lock() {
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    let bin = pack_gh(&fx);
    let before_bin = fs::read(&bin).unwrap();
    let lock_path = KitDirs::rooted(fx.root()).config.join("lock.toml");
    let before_lock = fs::read(&lock_path).unwrap();
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        Some("0".repeat(64)),
        "#!/bin/sh\necho gh version 9.9.9\n",
    );

    let report = fx.run(false, Some("gh")).unwrap();

    assert!(
        fx.shell.lines().iter().all(|line| !line.starts_with("tar")),
        "an unverified archive was unpacked: {:?}",
        fx.shell.lines()
    );
    assert_eq!(fs::read(&bin).unwrap(), before_bin, "the pack copy changed");
    assert_eq!(
        fs::read(&lock_path).unwrap(),
        before_lock,
        "the lock changed"
    );
    let reason = match &row(&report, "gh").result {
        RowResult::NotTouched(reason) => reason.clone(),
        other => panic!("expected NotTouched, got {other:?}"),
    };
    assert!(reason.contains("checksum mismatch"), "{reason}");
    assert_eq!(report.failed, vec!["gh".to_string()]);
}

#[test]
fn a_failed_gh_probe_restores_the_previous_copy_and_is_rolled_back() {
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    let bin = pack_gh(&fx);
    let before_bin = fs::read(&bin).unwrap();
    let lock_path = KitDirs::rooted(fx.root()).config.join("lock.toml");
    let before_lock = fs::read(&lock_path).unwrap();
    serve_gh(&fx, "9.9.9", b"archive", None, "#!/bin/sh\nbroken\n");
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::fail(1, "broken"),
    );

    let report = fx.run(false, Some("gh")).unwrap();

    assert!(
        matches!(row(&report, "gh").result, RowResult::RolledBack),
        "{:?}",
        row(&report, "gh")
    );
    assert_eq!(
        fs::read(&bin).unwrap(),
        before_bin,
        "the previous copy was not restored"
    );
    assert_eq!(
        fs::read(&lock_path).unwrap(),
        before_lock,
        "the lock changed"
    );
    assert_eq!(report.failed, vec!["gh".to_string()]);
}

#[test]
fn a_brew_gh_is_never_touched_by_an_update() {
    // The lock records the pack copy; the gh on the PATH is Homebrew's. Only the pack copy is written.
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    fx.shell.program("gh", Path::new("/opt/homebrew/bin/gh"));
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    let bin = KitDirs::rooted(fx.root()).bin.join("gh");
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );

    fx.run(false, Some("gh")).unwrap();

    assert!(
        fx.shell
            .lines()
            .iter()
            .all(|line| !line.contains("/opt/homebrew")),
        "{:?}",
        fx.shell.lines()
    );
}

#[test]
fn undo_puts_back_the_previous_gh_copy_and_its_lock_entry() {
    // The gh copy and the lock entry are journaled like rtk's pack copy, so undo restores both.
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    let bin = pack_gh(&fx);
    let before_bin = fs::read(&bin).unwrap();
    let lock_path = KitDirs::rooted(fx.root()).config.join("lock.toml");
    let before_lock = fs::read(&lock_path).unwrap();
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );
    fx.run(false, Some("gh")).unwrap();
    assert_ne!(
        fs::read(&bin).unwrap(),
        before_bin,
        "the update did not move gh"
    );

    undo(&fx.ctx(), Some(&fx.run)).unwrap();

    assert_eq!(
        fs::read(&bin).unwrap(),
        before_bin,
        "the previous gh copy was not restored"
    );
    assert_eq!(
        fs::read(&lock_path).unwrap(),
        before_lock,
        "the previous lock entry was not restored"
    );
}

#[test]
fn a_gh_move_from_an_unversioned_entry_is_recorded_not_updated() {
    // The lock never recorded gh's version, so the move records it: the row says so, and does not claim an update.
    let fx = Fixture::new();
    fx.lock(&[("gh", "unversioned", "release", &[])]);
    let bin = pack_gh(&fx);
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );

    let report = fx.run(false, Some("gh")).unwrap();

    let row = row(&report, "gh");
    assert!(matches!(row.result, RowResult::Recorded), "{row:?}");
    assert_eq!(fx.read_lock().tools["gh"].version, "9.9.9");
    assert!(report.failed.is_empty());
}

#[test]
fn a_link_at_the_gh_pack_path_is_not_written_through() {
    // The pack path is a link to a file elsewhere. The update never writes through it: the target and the lock stay
    // byte-identical, the link stays, and the row is not Updated.
    let fx = Fixture::new();
    fx.lock(&[("gh", "9.9.8", "release", &[])]);
    let target = fx.root().join("elsewhere").join("gh");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "elsewhere bytes\n").unwrap();
    let bin = KitDirs::rooted(fx.root()).bin.join("gh");
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &bin).unwrap();
    let before_target = fs::read(&target).unwrap();
    let lock_path = KitDirs::rooted(fx.root()).config.join("lock.toml");
    let before_lock = fs::read(&lock_path).unwrap();
    serve_gh(
        &fx,
        "9.9.9",
        b"archive",
        None,
        "#!/bin/sh\necho gh version 9.9.9\n",
    );
    fx.shell.on(
        &format!("{} --version", bin.display()),
        Output::ok("gh version 9.9.9 (2026-01-01)\n"),
    );

    let report = fx.run(false, Some("gh")).unwrap();

    assert_eq!(
        fs::read(&target).unwrap(),
        before_target,
        "the link target changed"
    );
    assert_eq!(
        fs::read(&lock_path).unwrap(),
        before_lock,
        "the lock changed"
    );
    assert!(
        bin.symlink_metadata().unwrap().file_type().is_symlink(),
        "the link was replaced"
    );
    let reason = match &row(&report, "gh").result {
        RowResult::NotTouched(reason) => reason.clone(),
        other => panic!("expected NotTouched, got {other:?}"),
    };
    assert!(reason.contains("link"), "{reason}");
    assert!(report.failed.is_empty(), "{report:?}");
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
    let lines = relaxkit::commands::update::lines(&report, relaxkit::ui::Palette::PLAIN);
    assert!(
        lines.last().is_some_and(|line| {
            line.starts_with("!  restart Claude Code") && line.contains("hooks and plugins")
        }),
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
    // The configuration steps run again after the binary changed.
    assert!(lines.iter().any(|l| l == "rtk init -g"), "{lines:?}");
}

#[test]
fn dangling_pack_rtk_link_is_removed_before_the_write() {
    // A dangling link at the pack rtk path. The release must be written as a regular
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
    // failure of the run. The stubs must see no mutating call and the home must not change.
    let scratch = Scratch::new();
    let before = tree(scratch.home());

    let text = stdout_of(&scratch, &["update", "--check"]);

    assert_eq!(tree(scratch.home()), before, "a check changed a file");
    assert!(
        scratch.calls().iter().all(|call| !mutating(call)),
        "a check ran a mutating command: {:?}",
        scratch.calls()
    );
    // Offline, the self step reports the newest release as unknown: it names the failed request and touches nothing.
    assert!(text.contains("newest unknown"), "{text}");
    assert!(text.contains("not touched"), "{text}");
}

#[test]
fn flags_select_scope_at_the_command_line() {
    let scratch = Scratch::new();

    // A plain update names the self step, which reports the newest release (unknown offline here). `--tools` leaves it out.
    let plain = stdout_of(&scratch, &["update", "--check"]);
    let tools_only = stdout_of(&scratch, &["update", "--check", "--tools"]);
    assert!(plain.contains("newest unknown"), "{plain}");
    assert!(!tools_only.contains("newest unknown"), "{tools_only}");

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
