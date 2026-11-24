//! The Claude Code adapter: the plugin manifest, the marketplace file, and the install argv.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use relaxkit::adapters::{install_claude, install_opencode};
use relaxkit::brand;
use relaxkit::ctx::Ctx;
use relaxkit::exec::{FakeRunner, Output};
use relaxkit::host::claude::ClaudeHost;
use relaxkit::host::opencode::OpencodeHost;
use relaxkit::host::{Host, HostInfo, HostKind, InstalledPlugin};
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
            "claude plugin marketplace list".to_string(),
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

/// The OpenCode install spec for release tag v0.1.0.
const SPEC_V010: &str = "relaxkit-opencode@git+https://github.com/cpenaforte/relaxkit.git#v0.1.0::path:adapters/opencode-plugin";

/// An OpenCode host that lists the plugins it is given and records each install and update it receives.
struct FakeOpencode {
    info: HostInfo,
    listed: Vec<InstalledPlugin>,
    calls: RefCell<Vec<String>>,
}

impl FakeOpencode {
    fn new(listed: Vec<InstalledPlugin>) -> FakeOpencode {
        FakeOpencode {
            info: HostInfo {
                kind: HostKind::Opencode,
                binary: "/usr/bin/opencode".into(),
                version: "2.2.6".into(),
            },
            listed,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Host for FakeOpencode {
    fn info(&self) -> &HostInfo {
        &self.info
    }

    fn plugin_install(&self, _ctx: &Ctx, spec: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("install {spec}"));
        Ok(())
    }

    fn plugin_update(&self, _ctx: &Ctx, spec: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("update {spec}"));
        Ok(())
    }

    fn plugin_list(&self, _ctx: &Ctx) -> Result<Vec<InstalledPlugin>> {
        Ok(self.listed.clone())
    }

    fn instructions_file(&self, _ctx: &Ctx) -> PathBuf {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn config_file(&self, _ctx: &Ctx) -> PathBuf {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn mcp_add(
        &self,
        _ctx: &Ctx,
        _name: &str,
        _argv: &[String],
        _env: &[(String, String)],
    ) -> Result<()> {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn mcp_remove(&self, _ctx: &Ctx, _name: &str) -> Result<()> {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn mcp_entry_present(&self, _ctx: &Ctx, _name: &str) -> Result<bool> {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn marketplace_add(&self, _ctx: &Ctx, _source: &str) -> Result<()> {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn marketplace_update(&self, _ctx: &Ctx) -> Result<()> {
        unreachable!("the OpenCode adapter does not use this")
    }

    fn state_files(&self, _ctx: &Ctx) -> Vec<PathBuf> {
        unreachable!("the OpenCode adapter does not use this")
    }
}

#[test]
fn opencode_adapter_install_argv_pins_the_release_tag() {
    let root = tempfile::tempdir().unwrap();
    let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let host = FakeOpencode::new(Vec::new());

    install_opencode(&ctx, &ctx.run, &host, "v0.1.0").unwrap();

    assert_eq!(
        *host.calls.borrow(),
        vec![format!("install {SPEC_V010}")],
        "the spec must be the pinned release tag"
    );
    // The tag is `#v` and a version, never a branch name.
    let tag = SPEC_V010
        .split_once('#')
        .unwrap()
        .1
        .split("::")
        .next()
        .unwrap();
    assert!(
        tag.strip_prefix('v')
            .is_some_and(|version| semver::Version::parse(version).is_ok()),
        "{tag}"
    );
    assert!(!SPEC_V010.contains("main") && !SPEC_V010.contains("master"));
}

#[test]
fn opencode_adapter_is_not_installed_twice() {
    let root = tempfile::tempdir().unwrap();
    let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let listed = vec![InstalledPlugin {
        name: "relaxkit-opencode".into(),
        marketplace: None,
        version: Some("0.1.0::path:adapters/opencode-plugin".into()),
    }];
    let host = FakeOpencode::new(listed);

    install_opencode(&ctx, &ctx.run, &host, "v0.1.0").unwrap();

    assert!(host.calls.borrow().is_empty(), "{:?}", host.calls.borrow());
}

#[test]
fn opencode_adapter_moves_to_a_new_tag() {
    let root = tempfile::tempdir().unwrap();
    let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
    let listed = vec![InstalledPlugin {
        name: "relaxkit-opencode".into(),
        marketplace: None,
        version: Some("0.0.9::path:adapters/opencode-plugin".into()),
    }];
    let host = FakeOpencode::new(listed);

    install_opencode(&ctx, &ctx.run, &host, "v0.1.0").unwrap();

    assert_eq!(*host.calls.borrow(), vec![format!("update {SPEC_V010}")]);
}

#[test]
fn opencode_adapter_reads_its_own_entry_from_the_real_config() {
    // The version the host reads back from `plugins` is the value the adapter compares, so a second run against the
    // real config file installs nothing.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("opencode");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("opencode.json"),
        format!("{{\"plugins\": [\"{SPEC_V010}\"]}}\n"),
    )
    .unwrap();
    let host = OpencodeHost::new(
        HostInfo {
            kind: HostKind::Opencode,
            binary: "/usr/bin/opencode".into(),
            version: "2.2.6".into(),
        },
        dir,
    );
    let fake = FakeRunner::new();
    let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());

    install_opencode(&ctx, &ctx.run, &host, "v0.1.0").unwrap();

    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}
