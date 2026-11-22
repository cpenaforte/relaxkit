//! Coding-agent hosts that relaxkit configures.

pub mod claude;
pub mod kind;
pub mod opencode;

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use semver::Version;

use crate::ctx::Ctx;
use crate::edit::jsonc_get;
use crate::exec::{Cmd, Output};
use crate::state::{RunId, safe_write};

pub use kind::HostKind;

/// A host binary found on the PATH, with the version it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub kind: HostKind,
    pub binary: PathBuf,
    pub version: String,
}

/// One plugin as the host reports it. `marketplace` is set only for `name@marketplace` specs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    pub marketplace: Option<String>,
    pub version: Option<String>,
}

/// Whether a host meets a recipe's `minimum_host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheck {
    Ok,
    TooOld { have: String, need: String },
}

/// The operations relaxkit needs from a native host CLI.
pub trait Host {
    fn info(&self) -> &HostInfo;
    fn instructions_file(&self, ctx: &Ctx) -> PathBuf;
    fn config_file(&self, ctx: &Ctx) -> PathBuf;
    fn mcp_add(
        &self,
        ctx: &Ctx,
        name: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<()>;
    fn mcp_remove(&self, ctx: &Ctx, name: &str) -> Result<()>;
    fn plugin_install(&self, ctx: &Ctx, spec: &str) -> Result<()>;
    fn plugin_update(&self, ctx: &Ctx, spec: &str) -> Result<()>;
    fn plugin_list(&self, ctx: &Ctx) -> Result<Vec<InstalledPlugin>>;
    fn marketplace_add(&self, ctx: &Ctx, source: &str) -> Result<()>;
}

/// Finds the hosts on the PATH. A host whose version cannot be read is left out with a warning.
/// The config locations honour `CLAUDE_CONFIG_DIR` and `XDG_CONFIG_HOME`, and are resolved here once.
pub fn detect_hosts(ctx: &Ctx) -> Vec<Box<dyn Host>> {
    let mut hosts: Vec<Box<dyn Host>> = Vec::new();
    for kind in [HostKind::Claude, HostKind::Opencode] {
        let name = kind.as_str();
        let Some(binary) = ctx.runner.which(name) else {
            continue;
        };
        let version = ctx
            .runner
            .run(&Cmd::new(name).arg("--version"))
            .ok()
            .filter(|out| out.code == 0)
            .and_then(|out| parse_version(&out.stdout));
        let Some(version) = version else {
            ctx.out.warn(&format!(
                "cannot read the version of {name}, so it is left out"
            ));
            continue;
        };
        let info = HostInfo {
            kind,
            binary,
            version,
        };
        let host: Box<dyn Host> = match kind {
            HostKind::Claude => {
                let (dir, state) = claude::claude_paths(
                    &ctx.dirs.home,
                    env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
                );
                Box::new(claude::ClaudeHost::new(info, dir, state))
            }
            HostKind::Opencode => {
                let dir = opencode::opencode_dir(
                    &ctx.dirs.home,
                    env::var_os("XDG_CONFIG_HOME").as_deref(),
                );
                Box::new(opencode::OpencodeHost::new(info, dir))
            }
        };
        hosts.push(host);
    }
    hosts
}

/// Reads the first version-shaped word of a `--version` line, such as `2.1.330` or `v2.2.6`.
pub fn parse_version(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|word| {
        let word = word.strip_prefix('v').unwrap_or(word);
        Version::parse(word).ok().map(|v| v.to_string())
    })
}

/// Compares a host version with a recipe's `minimum_host`. A version that cannot be read is not
/// known to be new enough, so it counts as too old.
pub fn check_minimum(info: &HostInfo, need: &str) -> HostCheck {
    match (Version::parse(&info.version), Version::parse(need)) {
        (Ok(have), Ok(want)) if have >= want => HostCheck::Ok,
        _ => HostCheck::TooOld {
            have: info.version.clone(),
            need: need.to_string(),
        },
    }
}

/// Names found directly under `mcp` in an OpenCode config, other than the V2 `servers` table.
/// Text that does not parse yields no names; the config editors report the parse error themselves.
pub fn v1_mcp_entries(text: &str) -> Vec<String> {
    match jsonc_get(text, &["mcp"]) {
        Ok(Some(serde_json::Value::Object(map))) => map
            .keys()
            .filter(|key| key.as_str() != "servers")
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// Fails when a host command exits non-zero, carrying its stderr, so a failed step is never reported as done.
fn check(cmd: &Cmd, out: Output) -> Result<Output> {
    if out.code != 0 {
        bail!(
            "`{cmd}` failed with exit {}: {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(out)
}

/// Runs a host command that only reads.
pub(crate) fn run_read(ctx: &Ctx, cmd: &Cmd) -> Result<Output> {
    check(cmd, ctx.runner.run(cmd)?)
}

/// Runs a host command that edits user files and returns its output whatever the exit code. Each existing
/// file is backed up and journaled through `state::safe_write` before the command runs. Its mode is
/// restored afterwards, because OpenCode's own writes change it (spike A1: 644 became 600). A dry run only
/// reports the command.
pub(crate) fn edit_guarded(ctx: &Ctx, files: &[PathBuf], cmd: &Cmd) -> Result<Output> {
    if ctx.dry_run {
        ctx.out.info(&format!("would run {cmd}"));
        return Ok(Output::ok(""));
    }
    let run = RunId::new();
    let mut modes = Vec::new();
    for file in files {
        let Ok(meta) = fs::metadata(file) else {
            continue;
        };
        let text =
            fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
        safe_write(ctx, &run, file, &text)?;
        modes.push((file, meta.permissions()));
    }
    let result = ctx.runner.run(cmd);
    for (file, perms) in modes {
        fs::set_permissions(file, perms)
            .with_context(|| format!("cannot restore the mode of {}", file.display()))?;
    }
    result
}

/// `edit_guarded` for a command that must succeed. A non-zero exit is an error with its stderr.
pub(crate) fn run_edit(ctx: &Ctx, files: &[PathBuf], cmd: &Cmd) -> Result<()> {
    check(cmd, edit_guarded(ctx, files, cmd)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;

    fn ctx_with(root: &Path, fake: &FakeRunner) -> Ctx {
        Ctx::for_test(root, fake.clone(), FakeNet::new())
    }

    fn info(kind: HostKind, version: &str) -> HostInfo {
        HostInfo {
            kind,
            binary: PathBuf::from("/bin/x"),
            version: version.to_string(),
        }
    }

    #[test]
    fn detect_hosts_reads_versions() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new()
            .with_program("claude", Path::new("/usr/bin/claude"))
            .with_program("opencode", Path::new("/usr/bin/opencode"))
            .reply("claude --version", Output::ok("2.1.330 (Claude Code)\n"))
            .reply("opencode --version", Output::ok("opencode v2.2.6\n"));
        let ctx = ctx_with(root.path(), &fake);
        let hosts = detect_hosts(&ctx);
        let versions: Vec<&str> = hosts.iter().map(|h| h.info().version.as_str()).collect();
        assert_eq!(versions, vec!["2.1.330", "2.2.6"]);
        assert_eq!(hosts[0].info().kind, HostKind::Claude);
        assert_eq!(hosts[1].info().binary, PathBuf::from("/usr/bin/opencode"));
    }

    #[test]
    fn missing_host_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new()
            .with_program("opencode", Path::new("/usr/bin/opencode"))
            .reply("opencode --version", Output::ok("opencode v2.2.6\n"));
        let ctx = ctx_with(root.path(), &fake);
        let hosts = detect_hosts(&ctx);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].info().kind, HostKind::Opencode);
    }

    #[test]
    fn unreadable_version_leaves_the_host_out_with_a_warning() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new()
            .with_program("claude", Path::new("/usr/bin/claude"))
            .reply("claude --version", Output::fail(1, "boom"));
        let ctx = ctx_with(root.path(), &fake);
        assert!(detect_hosts(&ctx).is_empty());
        assert!(ctx.out.lines().iter().any(|l| l.contains("claude")));
    }

    #[test]
    fn parse_version_reads_both_shapes() {
        assert_eq!(
            parse_version("2.1.330 (Claude Code)\n").as_deref(),
            Some("2.1.330")
        );
        assert_eq!(parse_version("opencode v2.2.6\n").as_deref(), Some("2.2.6"));
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn host_below_minimum_is_flagged() {
        assert_eq!(
            check_minimum(&info(HostKind::Opencode, "2.0.3"), "2.0.4"),
            HostCheck::TooOld {
                have: "2.0.3".into(),
                need: "2.0.4".into()
            }
        );
        assert_eq!(
            check_minimum(&info(HostKind::Opencode, "2.0.4"), "2.0.4"),
            HostCheck::Ok
        );
        assert_eq!(
            check_minimum(&info(HostKind::Claude, "2.1.330"), "2.1.330"),
            HostCheck::Ok
        );
    }

    #[test]
    fn v1_mcp_entries_finds_legacy_shape() {
        assert_eq!(
            v1_mcp_entries(r#"{"mcp":{"legacy":{"type":"local"}}}"#),
            vec!["legacy".to_string()]
        );
        assert!(v1_mcp_entries(r#"{"mcp":{"servers":{"x":{}}}}"#).is_empty());
        assert!(v1_mcp_entries("not json at all {").is_empty());
    }

    #[test]
    fn serena_recipe_argv_matches_the_rulings() {
        let recipes = crate::recipe::load_all().unwrap();
        let serena = recipes.iter().find(|r| r.id == "serena").unwrap();
        let mcp_argv = |steps: &[crate::recipe::Step]| -> Vec<String> {
            steps
                .iter()
                .find_map(|step| match step {
                    crate::recipe::Step::Mcp { argv, .. } => Some(argv.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let want_claude = "serena start-mcp-server --context claude-code --project-from-cwd --enable-web-dashboard False";
        let want_opencode =
            "serena start-mcp-server --context ide --project-from-cwd --enable-web-dashboard False";
        for map in [&serena.install, &serena.update] {
            assert_eq!(mcp_argv(&map[&HostKind::Claude]).join(" "), want_claude);
            assert_eq!(mcp_argv(&map[&HostKind::Opencode]).join(" "), want_opencode);
        }
    }
}
