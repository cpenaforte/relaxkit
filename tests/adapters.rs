//! The Claude Code adapter (Task 24, part A): the plugin manifest, the marketplace file, and the install argv.

use std::fs;
use std::path::Path;

use relaxkit::adapters::install_claude;
use relaxkit::brand;
use relaxkit::ctx::Ctx;
use relaxkit::exec::{FakeRunner, Output};
use relaxkit::host::claude::ClaudeHost;
use relaxkit::host::{HostInfo, HostKind};
use relaxkit::net::FakeNet;

fn repo_file(relative: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

#[test]
fn plugin_manifest_is_valid() {
    let manifest = repo_file("adapters/claude-plugin/.claude-plugin/plugin.json");
    assert_eq!(manifest["name"], brand::NAME, "{manifest}");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"), "{manifest}");

    // The marketplace at the repository root lists the plugin by the same name.
    let marketplace = repo_file(".claude-plugin/marketplace.json");
    let listed = marketplace["plugins"]
        .as_array()
        .expect("marketplace.json has a plugins list")
        .iter()
        .any(|plugin| plugin["name"] == manifest["name"]);
    assert!(listed, "{marketplace}");
}

fn claude_host(root: &Path) -> ClaudeHost {
    ClaudeHost::new(
        HostInfo {
            kind: HostKind::Claude,
            binary: "/usr/bin/claude".into(),
            version: "2.1.330".into(),
        },
        root.join(".claude"),
        root.join(".claude.json"),
    )
}

#[test]
fn claude_adapter_install_argv() {
    let root = tempfile::tempdir().unwrap();
    let plugin = format!("{}@{}", brand::NAME, brand::NAME);
    let host = claude_host(root.path());

    // First run: the plugin is not listed, so the marketplace is added and the plugin installed, in that order.
    let fresh = FakeRunner::new()
        .reply("claude plugin list", Output::ok(""))
        .reply("claude plugin marketplace list", Output::ok(""))
        .reply("claude plugin", Output::ok(""));
    let ctx = Ctx::for_test(root.path(), fresh.clone(), FakeNet::new());
    install_claude(&ctx, &ctx.run, &host).unwrap();
    assert_eq!(
        fresh.calls(),
        vec![
            "claude plugin list".to_string(),
            "claude plugin marketplace list".to_string(),
            format!("claude plugin marketplace add {}", brand::REPO),
            format!("claude plugin install {plugin}"),
        ]
    );

    // Second run: the plugin is listed, so nothing is added or installed again.
    let listed = FakeRunner::new()
        .reply(
            "claude plugin list",
            Output::ok(&format!("{plugin}  Version: 0.1.0\n")),
        )
        .reply("claude plugin", Output::ok(""));
    let ctx = Ctx::for_test(root.path(), listed.clone(), FakeNet::new());
    install_claude(&ctx, &ctx.run, &host).unwrap();
    assert_eq!(listed.calls(), vec!["claude plugin list".to_string()]);
}
