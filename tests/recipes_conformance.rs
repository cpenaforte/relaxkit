//! Recipe conformance (spec section 5.1): each pack tool, on each host, expands to the exact argv list the spec
//! names. Commands are recorded as argv tokens, so assertions do not depend on shell quoting. Every host command
//! is answered by a recording runner; nothing reaches the real machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use relaxkit::ctx::{Ctx, Reporter};
use relaxkit::exec::{Cmd, Output, Runner};
use relaxkit::host::claude::ClaudeHost;
use relaxkit::host::opencode::OpencodeHost;
use relaxkit::host::{Host, HostInfo, HostKind};
use relaxkit::net::FakeNet;
use relaxkit::platform::{KitDirs, chromium_candidates, playwright_cache_dir};
use relaxkit::recipe::load_all;
use relaxkit::runtime::nvm::{NodeInstall, npx_bin};
use relaxkit::state::RunId;
use relaxkit::state::lockfile::Lockfile;
use relaxkit::tools::{Engine, Outcome};

/// Records every command as argv tokens and answers from a list of prefixes. Unmatched commands succeed silently.
#[derive(Clone, Default)]
struct Recording {
    state: Arc<Mutex<RecState>>,
}

#[derive(Default)]
struct RecState {
    calls: Vec<Vec<String>>,
    replies: Vec<(String, Output)>,
    programs: HashMap<String, PathBuf>,
}

impl Recording {
    fn reply(&self, prefix: &str, output: Output) {
        self.state
            .lock()
            .unwrap()
            .replies
            .push((prefix.to_string(), output));
    }

    fn with_program(&self, name: &str, path: &str) {
        self.state
            .lock()
            .unwrap()
            .programs
            .insert(name.to_string(), PathBuf::from(path));
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.state.lock().unwrap().calls.clone()
    }
}

impl Runner for Recording {
    fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
        let mut argv = vec![cmd.program().to_string()];
        argv.extend(cmd.argv().iter().cloned());
        let display = argv.join(" ");
        let mut state = self.state.lock().unwrap();
        state.calls.push(argv);
        let out = state
            .replies
            .iter()
            .find(|(prefix, _)| display.starts_with(prefix.as_str()))
            .map(|(_, output)| output.clone())
            .unwrap_or_else(|| Output::ok(""));
        Ok(out)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.state.lock().unwrap().programs.get(program).cloned()
    }
}

/// One isolated machine: a temporary home, a recording runner, and the context built on them.
struct Rig {
    root: tempfile::TempDir,
    rec: Recording,
    ctx: Ctx,
}

fn rig() -> Rig {
    let root = tempfile::tempdir().unwrap();
    let rec = Recording::default();
    let ctx = Ctx {
        dirs: KitDirs::rooted(root.path()),
        runner: Box::new(rec.clone()),
        net: Box::new(FakeNet::new()),
        out: Reporter::capture(),
        dry_run: false,
        project: None,
        run: RunId::new(),
    };
    Rig { root, rec, ctx }
}

fn tokens(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

fn host(rig: &Rig, kind: HostKind) -> Box<dyn Host> {
    let root = rig.root.path();
    match kind {
        HostKind::Claude => Box::new(ClaudeHost::new(
            HostInfo {
                kind,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.330".to_string(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )),
        HostKind::Opencode => Box::new(OpencodeHost::new(
            HostInfo {
                kind,
                binary: PathBuf::from("/usr/local/bin/opencode"),
                version: "2.2.6".to_string(),
            },
            root.join(".config/opencode"),
        )),
    }
}

type Installed = anyhow::Result<Vec<(HostKind, Outcome)>>;

/// Runs `install` for one recipe on one host. Returns the result and every command that was asked for.
fn install(rig: &Rig, id: &str, kind: HostKind) -> (Installed, Vec<Vec<String>>) {
    let recipes = load_all().unwrap();
    let recipe = recipes.iter().find(|r| r.id == id).expect("recipe exists");
    let hosts = vec![host(rig, kind)];
    let node = NodeInstall {
        version: "v24.22.3".to_string(),
        root: rig
            .root
            .path()
            .join(".local/share/relaxkit/node/versions/node/v24.22.3"),
    };
    let engine = Engine {
        ctx: &rig.ctx,
        run: &rig.ctx.run,
        hosts: &hosts,
        recipes: &[],
        node: &node,
    };
    let result = engine.install(recipe);
    (result, rig.rec.calls())
}

/// The state of a machine that already has uv on the PATH and the pack's rtk binary and Chromium in place.
fn prepared() -> Rig {
    let rig = rig();
    rig.rec.with_program("uv", "/usr/local/bin/uv");
    let bin = KitDirs::rooted(rig.root.path()).bin;
    write_executable(&bin.join("rtk"));
    write_executable(&chromium_binary(&rig));
    rig
}

fn write_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Where Playwright's Chromium executable sits for this OS, under the rig's home.
fn chromium_binary(rig: &Rig) -> PathBuf {
    let cache = playwright_cache_dir(rig.root.path()).unwrap();
    cache
        .join("chromium-1255")
        .join(chromium_candidates().unwrap()[0])
}

/// Lets the marketplace handler find the catalog of the built-in directory (a file on disk, as a marketplace has).
fn write_catalog(rig: &Rig, marketplace: &str, plugins: &[&str]) {
    let names: Vec<String> = plugins
        .iter()
        .map(|name| format!("{{\"name\":\"{name}\"}}"))
        .collect();
    let path = rig
        .root
        .path()
        .join(".claude/plugins/marketplaces")
        .join(marketplace)
        .join(".claude-plugin/marketplace.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "{{\"name\":\"{marketplace}\",\"plugins\":[{}]}}",
            names.join(",")
        ),
    )
    .unwrap();
}

/// The PATH every MCP entry carries: the node link, then the system directories (spec 4).
fn mcp_path(rig: &Rig) -> String {
    let link = rig.ctx.dirs.data.join("node").join("bin");
    format!("PATH={}:/usr/bin:/bin", link.display())
}

fn npx(rig: &Rig) -> String {
    npx_bin(&rig.ctx).display().to_string()
}

fn npm(rig: &Rig) -> String {
    rig.ctx.dirs.data.join("node/bin/npm").display().to_string()
}

fn expect_calls(actual: &[Vec<String>], expected: Vec<Vec<String>>) {
    assert_eq!(actual, expected.as_slice());
}

#[test]
fn superpowers_on_claude_installs_from_the_built_in_directory() {
    let rig = prepared();
    let (result, calls) = install(&rig, "superpowers", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    expect_calls(
        &calls,
        vec![
            tokens(&["claude", "plugin", "list"]),
            tokens(&[
                "claude",
                "plugin",
                "install",
                "superpowers@anthropic-plugin-directory",
            ]),
        ],
    );
}

#[test]
fn superpowers_on_opencode_pins_the_git_tag_under_plugins() {
    let rig = prepared();
    let (result, calls) = install(&rig, "superpowers", HostKind::Opencode);
    assert!(result.is_ok(), "{result:?}");
    expect_calls(
        &calls,
        vec![tokens(&[
            "opencode",
            "plugin",
            "add",
            "superpowers@git+https://github.com/obra/superpowers.git#v7.0.2",
        ])],
    );
}

#[test]
fn rtk_on_claude_stops_at_the_unimplemented_hook_before_any_command() {
    let rig = prepared();
    let (result, calls) = install(&rig, "rtk", HostKind::Claude);
    let err = format!("{:#}", result.unwrap_err());
    assert!(err.contains("rtk_claude_hook"), "{err}");
    assert!(calls.is_empty(), "{calls:?}");
}

#[test]
fn rtk_on_opencode_runs_init_then_telemetry_off_and_never_brew() {
    let rig = prepared();
    let (result, calls) = install(&rig, "rtk", HostKind::Opencode);
    assert!(result.is_ok(), "{result:?}");
    expect_calls(
        &calls,
        vec![
            tokens(&["rtk", "init", "-g", "--opencode"]),
            tokens(&["rtk", "telemetry", "disable"]),
        ],
    );
    assert!(calls.iter().all(|call| call[0] != "brew"), "{calls:?}");
}

#[test]
fn caveman_on_claude_adds_the_marketplace_then_installs_the_cli_and_turns_telemetry_off() {
    let rig = prepared();
    let (result, calls) = install(&rig, "caveman", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    expect_calls(
        &calls,
        vec![
            tokens(&["claude", "plugin", "list"]),
            tokens(&["claude", "plugin", "marketplace", "list"]),
            tokens(&[
                "claude",
                "plugin",
                "marketplace",
                "add",
                "JuliusBrussee/caveman",
            ]),
            tokens(&["claude", "plugin", "install", "caveman@caveman"]),
            vec![
                npm(&rig),
                "install".into(),
                "-g".into(),
                "@caveman-ai/cli@4.1.0".into(),
            ],
            tokens(&["caveman", "off", "routing", "--yes"]),
            tokens(&["caveman", "telemetry", "off"]),
        ],
    );
}

#[test]
fn caveman_on_opencode_stops_at_the_unimplemented_opencode_handler() {
    let rig = prepared();
    let (result, calls) = install(&rig, "caveman", HostKind::Opencode);
    let err = format!("{:#}", result.unwrap_err());
    assert!(err.contains("caveman_opencode"), "{err}");
    assert!(calls.is_empty(), "{calls:?}");
}

#[test]
fn caveman_does_not_add_a_marketplace_that_is_already_present() {
    let rig = prepared();
    rig.rec.reply(
        "claude plugin marketplace list",
        Output::ok("❯ caveman   Source: GitHub (JuliusBrussee/caveman)\n"),
    );
    let (result, calls) = install(&rig, "caveman", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    assert!(
        calls.iter().all(|call| !call.contains(&"add".to_string())),
        "{calls:?}"
    );
}

#[test]
fn playwright_on_claude_installs_chromium_then_writes_the_pinned_entry() {
    let rig = prepared();
    let (result, calls) = install(&rig, "playwright", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    let npx = npx(&rig);
    expect_calls(
        &calls,
        vec![
            vec![
                npx.clone(),
                "-y".into(),
                "@playwright/mcp@0.0.86".into(),
                "install-browser".into(),
                "chromium".into(),
            ],
            tokens(&["claude", "mcp", "remove", "--scope", "user", "playwright"]),
            vec![
                "claude".into(),
                "mcp".into(),
                "add".into(),
                "--scope".into(),
                "user".into(),
                "playwright".into(),
                "--env".into(),
                mcp_path(&rig),
                "--".into(),
                npx,
                "-y".into(),
                "@playwright/mcp@0.0.86".into(),
                "--browser".into(),
                "chromium".into(),
                "--isolated".into(),
            ],
        ],
    );
}

#[test]
fn playwright_on_opencode_installs_chromium_then_writes_the_pinned_entry() {
    let rig = prepared();
    let (result, calls) = install(&rig, "playwright", HostKind::Opencode);
    assert!(result.is_ok(), "{result:?}");
    let npx = npx(&rig);
    expect_calls(
        &calls,
        vec![
            vec![
                npx.clone(),
                "-y".into(),
                "@playwright/mcp@0.0.86".into(),
                "install-browser".into(),
                "chromium".into(),
            ],
            vec![
                "opencode".into(),
                "mcp".into(),
                "add".into(),
                "--global".into(),
                "--env".into(),
                mcp_path(&rig),
                "playwright".into(),
                "--".into(),
                npx,
                "-y".into(),
                "@playwright/mcp@0.0.86".into(),
                "--browser".into(),
                "chromium".into(),
                "--isolated".into(),
            ],
        ],
    );
}

#[test]
fn chrome_devtools_on_claude_uses_playwright_chromium_and_no_update_check() {
    let rig = prepared();
    let (result, calls) = install(&rig, "chrome-devtools", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    let npx = npx(&rig);
    let chromium = chromium_binary(&rig).display().to_string();
    expect_calls(
        &calls,
        vec![
            tokens(&[
                "claude",
                "mcp",
                "remove",
                "--scope",
                "user",
                "chrome-devtools",
            ]),
            vec![
                "claude".into(),
                "mcp".into(),
                "add".into(),
                "--scope".into(),
                "user".into(),
                "chrome-devtools".into(),
                "--env".into(),
                mcp_path(&rig),
                "--env".into(),
                "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS=1".into(),
                "--".into(),
                npx,
                "-y".into(),
                "chrome-devtools-mcp@1.13.0".into(),
                "--executablePath".into(),
                chromium,
                "--no-usage-statistics".into(),
                "--no-performance-crux".into(),
            ],
        ],
    );
}

#[test]
fn chrome_devtools_on_opencode_uses_the_same_entry_shape() {
    let rig = prepared();
    let (result, calls) = install(&rig, "chrome-devtools", HostKind::Opencode);
    assert!(result.is_ok(), "{result:?}");
    let npx = npx(&rig);
    let chromium = chromium_binary(&rig).display().to_string();
    expect_calls(
        &calls,
        vec![vec![
            "opencode".into(),
            "mcp".into(),
            "add".into(),
            "--global".into(),
            "--env".into(),
            mcp_path(&rig),
            "--env".into(),
            "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS=1".into(),
            "chrome-devtools".into(),
            "--".into(),
            npx,
            "-y".into(),
            "chrome-devtools-mcp@1.13.0".into(),
            "--executablePath".into(),
            chromium,
            "--no-usage-statistics".into(),
            "--no-performance-crux".into(),
        ]],
    );
}

/// Serena sets a workdir, which the Host API cannot apply (SA-1, owner decision pending). Until the owner decides,
/// its install is refused before any command runs, on both hosts.
#[test]
fn serena_is_refused_before_any_command_while_the_workdir_decision_is_pending() {
    for kind in [HostKind::Claude, HostKind::Opencode] {
        let rig = prepared();
        let (result, calls) = install(&rig, "serena", kind);
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("workdir not applied (SA-1, owner decision pending): serena"),
            "{err}"
        );
        assert!(calls.is_empty(), "{kind}: {calls:?}");
    }
}

#[test]
fn ccusage_has_no_host_registration_and_the_lock_records_the_pin() {
    let rig = prepared();
    for kind in [HostKind::Claude, HostKind::Opencode] {
        let (result, calls) = install(&rig, "ccusage", kind);
        assert!(result.is_ok(), "{result:?}");
        assert!(calls.is_empty(), "{calls:?}");
    }
    let lock = Lockfile::load(&rig.ctx).unwrap().unwrap();
    assert_eq!(lock.tools["ccusage"].version, "20.0.51");
}

#[test]
fn claude_md_management_resolves_its_marketplace_from_the_catalog() {
    let rig = prepared();
    write_catalog(
        &rig,
        "anthropic-plugin-directory",
        &["claude-md-management"],
    );
    rig.rec.reply(
        "claude plugin marketplace list",
        Output::ok("❯ anthropic-plugin-directory   Source: Built in (Anthropic Directory)\n"),
    );
    let (result, calls) = install(&rig, "claude-md-management", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    expect_calls(
        &calls,
        vec![
            tokens(&["claude", "plugin", "list"]),
            tokens(&["claude", "plugin", "list"]),
            tokens(&["claude", "plugin", "marketplace", "list"]),
            tokens(&[
                "claude",
                "plugin",
                "install",
                "claude-md-management@anthropic-plugin-directory",
            ]),
        ],
    );
}

#[test]
fn frontend_design_resolves_its_marketplace_from_the_catalog() {
    let rig = prepared();
    write_catalog(&rig, "anthropic-plugin-directory", &["frontend-design"]);
    rig.rec.reply(
        "claude plugin marketplace list",
        Output::ok("❯ anthropic-plugin-directory   Source: Built in (Anthropic Directory)\n"),
    );
    let (result, calls) = install(&rig, "frontend-design", HostKind::Claude);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        calls.last().unwrap(),
        &tokens(&[
            "claude",
            "plugin",
            "install",
            "frontend-design@anthropic-plugin-directory"
        ])
    );
}

#[test]
fn an_unlisted_plugin_fails_closed_and_installs_nothing() {
    let rig = prepared();
    rig.rec.reply(
        "claude plugin marketplace list",
        Output::ok("❯ anthropic-plugin-directory   Source: Built in (Anthropic Directory)\n"),
    );
    let (result, calls) = install(&rig, "frontend-design", HostKind::Claude);
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("no Claude marketplace lists `frontend-design`"),
        "{err}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.contains(&"install".to_string())),
        "{calls:?}"
    );
}

#[test]
fn opencode_sandbox_pins_the_plugin_then_stops_at_its_unimplemented_handler() {
    let rig = prepared();
    let (result, calls) = install(&rig, "opencode-sandbox", HostKind::Opencode);
    let err = format!("{:#}", result.unwrap_err());
    assert!(err.contains("opencode_sandbox_config"), "{err}");
    assert!(calls.is_empty(), "{calls:?}");
}

#[test]
fn no_recipe_ever_runs_a_forbidden_command() {
    let forbidden = [
        "cargo install rtk",
        "DO_NOT_TRACK",
        "caveman login",
        "caveman providers",
    ];
    let mut checked = 0;
    for recipe in load_all().unwrap() {
        for &kind in &recipe.hosts {
            let rig = prepared();
            let _ = install(&rig, &recipe.id, kind);
            for call in rig.rec.calls() {
                let line = call.join(" ");
                for needle in forbidden {
                    assert!(
                        !line.contains(needle),
                        "{} on {kind} ran `{line}`",
                        recipe.id
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}
