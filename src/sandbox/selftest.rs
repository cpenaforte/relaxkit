//! The sandbox self-test (spec 7, spike B). It drives the pinned sandbox runtime through the Node link and checks
//! three things the sandbox must do: refuse a write outside the project, refuse a direct connection that skips the
//! proxy, and refuse a host that is not on the allowlist. Nothing here runs unless the caller asks for it.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use super::ALLOWED_DOMAINS;
use crate::ctx::Ctx;
use crate::exec::{Cmd, Output};
use crate::fsx::write_atomic;
use crate::host::Host;
use crate::runtime::nvm::npx_bin;
use crate::runtime::utf8;

/// The sandbox runtime package the self-test drives (spec 7, item 25). It is the version the OpenCode plugin bundles.
pub const RUNTIME_PACKAGE: &str = "@anthropic-ai/sandbox-runtime";
pub const RUNTIME_PIN: &str = "0.0.76";

/// The probe names, in the order `selftest` runs them.
pub const PROBES: [&str; 3] = [
    "write outside the project is refused",
    "direct connection bypassing the proxy is refused",
    "non-allowlisted host is refused",
];

/// One probe's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub probe: &'static str,
    pub passed: bool,
    pub detail: String,
}

/// Runs the three probes through the pinned runtime and reports each one. A probe that cannot run is a failure.
pub fn selftest(ctx: &Ctx, host: &dyn Host) -> Vec<ProbeResult> {
    let kind = host.info().kind;
    if ctx.dry_run {
        return PROBES
            .iter()
            .map(|probe| ProbeResult {
                probe,
                passed: false,
                detail: "not run in a dry run".to_string(),
            })
            .collect();
    }
    if let Err(err) = prepare(ctx) {
        return PROBES
            .iter()
            .map(|probe| ProbeResult {
                probe,
                passed: false,
                detail: format!("cannot prepare the self-test: {err:#}"),
            })
            .collect();
    }
    PROBES
        .iter()
        .enumerate()
        .map(|(index, probe)| {
            let (passed, detail) =
                match probe_command(ctx, index).and_then(|cmd| ctx.runner.run(&cmd)) {
                    Ok(out) => judge(index, &out),
                    Err(err) => (false, format!("the runtime did not run: {err:#}")),
                };
            ProbeResult {
                probe,
                passed,
                detail: format!("{kind}: {detail}"),
            }
        })
        .collect()
}

/// The command for probe `index`: the pinned runtime through the Node link, with the self-test's settings, run from
/// the self-test project directory. The probe's own shell script follows `sh -c`.
pub fn probe_command(ctx: &Ctx, index: usize) -> Result<Cmd> {
    let npx = utf8(&npx_bin(ctx))?.to_string();
    let package = format!("{RUNTIME_PACKAGE}@{RUNTIME_PIN}");
    let settings = utf8(&settings_file(ctx))?.to_string();
    let script = script(ctx, index)?;
    Ok(Cmd::new(&npx)
        .arg("--yes")
        .arg(&package)
        .arg("--settings")
        .arg(&settings)
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .cwd(&project_dir(ctx)))
}

/// The shell script of probe `index`. Each one prints `status=` with the exit status of its attempt, so a runtime that
/// did not start is never read as a refusal.
fn script(ctx: &Ctx, index: usize) -> Result<String> {
    Ok(match index {
        0 => format!(
            "echo probe > {}; echo \"status=$?\"",
            sh_quote(utf8(&outside_file(ctx))?)
        ),
        1 => "curl --noproxy '*' -sS -o /dev/null https://api.github.com/; echo \"status=$?\""
            .to_string(),
        _ => "curl -sS -o /dev/null -w '%{http_code}' https://example.com/; echo \"status=$?\""
            .to_string(),
    })
}

/// Whether probe `index` passed, from its output, and the reason in words.
fn judge(index: usize, out: &Output) -> (bool, String) {
    let status = out
        .stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("status=")?.parse::<i32>().ok());
    if index < 2 {
        return match status {
            None => (
                false,
                "the probe did not report its exit status".to_string(),
            ),
            Some(127) => (false, "the probe's command was not found".to_string()),
            Some(0) => (false, "the sandbox let the attempt succeed".to_string()),
            Some(code) => (true, format!("refused (exit {code})")),
        };
    }
    let code = out
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| line.len() == 3 && line.chars().all(|c| c.is_ascii_digit()));
    match (code, status) {
        (Some(code), Some(_)) if !code.starts_with('2') => (true, format!("refused (HTTP {code})")),
        (Some(code), Some(_)) => (false, format!("answered HTTP {code}")),
        _ => (false, "no HTTP status was reported".to_string()),
    }
}

/// Writes the self-test's settings (the curated allowlist, writes allowed only in the project directory) and makes
/// the project directory. These are relaxkit's own scratch files under its data directory, not a host's config.
fn prepare(ctx: &Ctx) -> Result<()> {
    fs::create_dir_all(project_dir(ctx))?;
    let settings = json!({
        "network": {"allowedDomains": ALLOWED_DOMAINS, "deniedDomains": []},
        "filesystem": {"denyRead": [], "allowWrite": [project_dir(ctx)], "denyWrite": []}
    });
    write_atomic(
        &settings_file(ctx),
        serde_json::to_string_pretty(&settings)?.as_bytes(),
    )
}

fn scratch_dir(ctx: &Ctx) -> PathBuf {
    ctx.dirs.data.join("selftest")
}

fn project_dir(ctx: &Ctx) -> PathBuf {
    scratch_dir(ctx).join("project")
}

fn settings_file(ctx: &Ctx) -> PathBuf {
    scratch_dir(ctx).join("srt-settings.json")
}

/// A file outside the project directory, which the write probe tries to create.
fn outside_file(ctx: &Ctx) -> PathBuf {
    scratch_dir(ctx).join("outside.txt")
}

/// Single-quotes a word for `sh -c`.
fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::host::opencode::OpencodeHost;
    use crate::host::{HostInfo, HostKind};
    use crate::net::FakeNet;

    fn host(root: &std::path::Path) -> OpencodeHost {
        OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/bin/opencode"),
                version: "2.2.6".into(),
            },
            root.join(".config/opencode"),
        )
    }

    /// The display string of probe `index`, which is the key a fake runner answers it with.
    fn key(ctx: &Ctx, index: usize) -> String {
        probe_command(ctx, index).unwrap().to_string()
    }

    #[test]
    fn selftest_has_three_probes() {
        let root = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new()
            .reply(
                &key(
                    &Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new()),
                    0,
                ),
                Output::ok("status=1\n"),
            )
            .reply(
                &key(
                    &Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new()),
                    1,
                ),
                Output::ok("status=56\n"),
            )
            .reply(
                &key(
                    &Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new()),
                    2,
                ),
                Output::ok("000\nstatus=56\n"),
            );
        let ctx = Ctx::for_test(root.path(), runner, FakeNet::new());

        let results = selftest(&ctx, &host(root.path()));

        assert_eq!(results.len(), 3, "{results:?}");
        let names: Vec<&str> = results.iter().map(|result| result.probe).collect();
        assert_eq!(names, PROBES);
        assert!(results.iter().all(|result| result.passed), "{results:?}");
    }

    #[test]
    fn a_probe_the_sandbox_lets_through_fails() {
        let root = tempfile::tempdir().unwrap();
        let scratch = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());
        let runner = FakeRunner::new()
            .reply(&key(&scratch, 0), Output::ok("status=0\n"))
            .reply(&key(&scratch, 1), Output::ok("status=56\n"))
            .reply(&key(&scratch, 2), Output::ok("200\nstatus=0\n"));
        let ctx = Ctx::for_test(root.path(), runner, FakeNet::new());

        let results = selftest(&ctx, &host(root.path()));

        assert!(!results[0].passed, "{results:?}");
        assert!(results[1].passed, "{results:?}");
        assert!(!results[2].passed, "{results:?}");
    }

    #[test]
    fn a_runtime_that_cannot_start_fails_every_probe() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());

        let results = selftest(&ctx, &host(root.path()));

        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|result| !result.passed), "{results:?}");
    }
}
