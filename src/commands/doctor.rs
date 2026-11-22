//! `relaxkit doctor` (spec sections 5 and 7, 10 items 11 and 15, 25): the checks that decide whether the pack is
//! sound. Read-only: nothing here rewrites a file. Each check has a stable id, its outcome, what it found, and a fix
//! when it is not ok (rulings K47, K50, K53).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::commands::read_optional;
use crate::commands::setup::sudo_command;
use crate::ctx::Ctx;
use crate::edit::jsonc_get;
use crate::exec::Cmd;
use crate::host::{Host, HostKind, detect_hosts, v1_mcp_entries};
use crate::platform::{
    BWRAP_PROFILE_PATH, PrereqPlan, apparmor_profile_present, is_executable,
    plan_needs_apparmor_profile,
};
use crate::recipe::{Kind, Step, load_all};
use crate::runtime::nvm::{node_bin, npx_bin};
use crate::sandbox::selftest::selftest;
use crate::sandbox::{linux_prerequisites, plugin_health};
use crate::state::lockfile::Lockfile;
use crate::tools::engine::{mcp_path_dirs, probe_path};

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    /// The word used in text and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

/// One check: a stable id, its outcome, what it found, and the fix when it is not ok.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub message: String,
    pub fix: Option<String>,
}

impl Check {
    fn ok(id: &str, message: impl Into<String>) -> Check {
        Check {
            id: id.to_string(),
            status: CheckStatus::Ok,
            message: message.into(),
            fix: None,
        }
    }

    fn warn(id: &str, message: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id: id.to_string(),
            status: CheckStatus::Warn,
            message: message.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(id: &str, message: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id: id.to_string(),
            status: CheckStatus::Fail,
            message: message.into(),
            fix: Some(fix.into()),
        }
    }
}

/// Runs every check on the hosts found on the PATH.
pub fn run(ctx: &Ctx) -> Result<Vec<Check>> {
    let hosts = detect_hosts(ctx);
    checks_for(ctx, &hosts)
}

/// Runs every check on the given hosts, in a fixed order.
pub fn checks_for(ctx: &Ctx, hosts: &[Box<dyn Host>]) -> Result<Vec<Check>> {
    let mut checks = vec![prerequisites_check(&linux_prerequisites(ctx)?, |program| {
        ctx.runner.which(program).is_some()
    })];
    for host in hosts {
        match host.info().kind {
            HostKind::Claude => claude_checks(ctx, host.as_ref(), &mut checks)?,
            HostKind::Opencode => opencode_checks(ctx, host.as_ref(), &mut checks)?,
        }
    }
    project_check(ctx, &mut checks);
    plugin_check(ctx, &mut checks)?;
    tool_probe_checks(ctx, hosts, &mut checks)?;
    mcp_checks(ctx, hosts, &mut checks)?;
    Ok(checks)
}

/// The sandbox's Linux packages (spec 7). On Linux, bubblewrap and socat must be on the PATH; the fix is the one sudo
/// command that installs them (ruling K47). A plan with no packages (every macOS plan) is ok.
pub fn prerequisites_check(plan: &PrereqPlan, have: impl Fn(&str) -> bool) -> Check {
    if plan.sudo.is_empty() {
        return Check::ok(
            "sandbox.prerequisites",
            "no Linux packages are needed on this system",
        );
    }
    let missing: Vec<&str> = ["bwrap", "socat"]
        .into_iter()
        .filter(|program| !have(program))
        .collect();
    // On Ubuntu 24.04 and later the AppArmor profile is part of the plan, so a check without it fails (ruling K57).
    let needs_profile = plan_needs_apparmor_profile(plan);
    let profile_missing = needs_profile && !apparmor_profile_present();
    let mut needs = Vec::new();
    if !missing.is_empty() {
        needs.push(format!("{} on the PATH", missing.join(" and ")));
    }
    if profile_missing {
        needs.push(format!("the AppArmor profile {BWRAP_PROFILE_PATH}"));
    }
    if needs.is_empty() {
        let message = if needs_profile {
            "bubblewrap, socat and the AppArmor profile are in place"
        } else {
            "bubblewrap and socat are installed"
        };
        return Check::ok("sandbox.prerequisites", message);
    }
    Check::fail(
        "sandbox.prerequisites",
        format!(
            "the sandbox needs {}, and it is missing",
            needs.join(" and ")
        ),
        format!(
            "run this once yourself (relaxkit does not run sudo without --allow-sudo): {}",
            sudo_command(plan)
        ),
    )
}

/// Claude Code's settings: that the file parses, that no base URL override is set, and that caveman's badge
/// did not replace the user's status line. Then the sandbox self-test.
fn claude_checks(ctx: &Ctx, host: &dyn Host, checks: &mut Vec<Check>) -> Result<()> {
    let path = host.config_file(ctx);
    let Some(text) = read_optional(&path)? else {
        checks.push(Check::ok("claude.settings", "no settings.json yet"));
        return selftest_check(ctx, host, "claude.sandbox.selftest", checks);
    };
    if let Err(err) = jsonc_get(&text, &["env"]) {
        checks.push(Check::fail(
            "claude.settings",
            format!("{} does not parse: {err:#}", path.display()),
            "relaxkit does not rewrite a file that does not parse; repair it by hand",
        ));
    } else {
        checks.push(Check::ok("claude.settings", "settings.json parses"));
    }
    // A warn, not a fail: relaxkit does not set this variable, and a user's own proxy sets it too (ruling OWNER-2).
    match jsonc_get(&text, &["env", "ANTHROPIC_BASE_URL"]).ok().flatten() {
        Some(_) => checks.push(Check::warn(
            "claude.anthropic_base_url",
            "ANTHROPIC_BASE_URL is set in settings.json. relaxkit does not set it; remove it if you do not use a proxy.",
            format!(
                "remove the ANTHROPIC_BASE_URL key from the env object in {}",
                path.display()
            ),
        )),
        None => checks.push(Check::ok("claude.anthropic_base_url", "ANTHROPIC_BASE_URL is not set")),
    }
    let command = jsonc_get(&text, &["statusLine", "command"])
        .ok()
        .flatten()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    if command.contains("caveman") {
        checks.push(Check::fail(
            "claude.status_line",
            format!("the statusLine is caveman's badge ({command}), which replaced the user's status line"),
            "restore your own statusLine in settings.json",
        ));
    } else {
        checks.push(Check::ok(
            "claude.status_line",
            "the statusLine is not caveman's",
        ));
    }
    selftest_check(ctx, host, "claude.sandbox.selftest", checks)
}

/// OpenCode's config: that it parses, and whether it still holds V1 `mcp.<name>` entries. Relaxkit reports them and
/// never rewrites them (Review Focus 1). Then the sandbox self-test.
fn opencode_checks(ctx: &Ctx, host: &dyn Host, checks: &mut Vec<Check>) -> Result<()> {
    let path = host.config_file(ctx);
    match read_optional(&path)? {
        None => checks.push(Check::ok("opencode.mcp_shape", "no OpenCode config yet")),
        Some(text) => {
            if let Err(err) = jsonc_get(&text, &["mcp"]) {
                checks.push(Check::fail(
                    "opencode.mcp_shape",
                    format!("{} does not parse: {err:#}", path.display()),
                    "relaxkit does not rewrite a file that does not parse; repair it by hand",
                ));
            } else {
                let names = v1_mcp_entries(&text);
                if names.is_empty() {
                    checks.push(Check::ok("opencode.mcp_shape", "no V1 mcp entries"));
                } else {
                    checks.push(Check::warn(
                        "opencode.mcp_shape",
                        format!("written in the V1 shape (mcp.<name>): {}", names.join(", ")),
                        "relaxkit does not rewrite your file; move each entry under mcp.servers by hand",
                    ));
                }
            }
        }
    }
    selftest_check(ctx, host, "opencode.sandbox.selftest", checks)
}

/// The sandbox self-test for one host (spec 7). A probe that is not refused fails the check. Nothing is run in a
/// dry run.
fn selftest_check(ctx: &Ctx, host: &dyn Host, id: &str, checks: &mut Vec<Check>) -> Result<()> {
    let kind = host.info().kind.product_name();
    if ctx.dry_run {
        // A dry run runs no self-test, so nothing is verified: a warn, never an ok (ruling R2-5).
        checks.push(Check::warn(
            id,
            format!("{kind}: not run in a dry run"),
            "run relaxkit doctor without --dry-run to run the sandbox self-test",
        ));
        return Ok(());
    }
    let results = selftest(ctx, host);
    let failed: Vec<String> = results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| format!("{}: {}", result.probe, result.detail))
        .collect();
    if failed.is_empty() {
        checks.push(Check::ok(
            id,
            format!("{kind}: the sandbox refused every probe"),
        ));
    } else {
        // A probe that did not run is not a refusal, so the message says the self-test could not run (ruling Important 4).
        let message = if results.iter().any(|result| !result.ran) {
            format!(
                "{kind}: sandbox self-test could not run: {}",
                failed.join("; ")
            )
        } else {
            format!("{kind}: the sandbox did not refuse: {}", failed.join("; "))
        };
        checks.push(Check::fail(
            id,
            message,
            "run `relaxkit setup` again; until every probe passes, the sandbox is not enforcing",
        ));
    }
    Ok(())
}

/// A sandbox file in the project would be read before relaxkit's global one (spec 10 item 15). Checked only when a
/// project is given.
fn project_check(ctx: &Ctx, checks: &mut Vec<Check>) {
    let Some(project) = &ctx.project else {
        return;
    };
    let file = project.join(".opencode").join("sandbox.json");
    if file.exists() {
        checks.push(Check::warn(
            "project.sandbox_file",
            format!(
                "{} is read before relaxkit's sandbox config",
                file.display()
            ),
            "delete it, or move its settings into the global sandbox config",
        ));
    } else {
        checks.push(Check::ok("project.sandbox_file", "no project sandbox file"));
    }
}

/// The sandbox plugin's age (spec 10 item 25). It warns only after 120 days without a release; runtime lag alone
/// never warns. An unreachable registry is not a failure.
fn plugin_check(ctx: &Ctx, checks: &mut Vec<Check>) -> Result<()> {
    let Some(lock) = Lockfile::load(ctx)? else {
        return Ok(());
    };
    let Some(tool) = lock.tools.get("opencode-sandbox") else {
        return Ok(());
    };
    let id = "plugin.opencode-sandbox";
    match plugin_health(ctx, &tool.version) {
        Ok(health) if health.release_age_days > STALE_AFTER_DAYS => checks.push(Check::warn(
            id,
            format!(
                "opencode-sandbox {} has had no release for {} days (over {STALE_AFTER_DAYS})",
                tool.version, health.release_age_days
            ),
            "watch for a release; if none comes, the contingency is a relaxkit-owned wrapper on @anthropic-ai/sandbox-runtime inside the OpenCode plugin (not built in v1)",
        )),
        Ok(health) => checks.push(Check::ok(
            id,
            format!(
                "latest release {} days ago; runtime lag {} (a lag alone never warns)",
                health.release_age_days, health.runtime_lag
            ),
        )),
        Err(err) => checks.push(Check::ok(id, format!("health unknown: {err:#}"))),
    }
    Ok(())
}

/// Days without a plugin release before doctor warns (spec 10 item 25).
const STALE_AFTER_DAYS: u32 = 120;

/// Each locked tool's probe, for each installed host it is locked on. A probe whose program is set at install time
/// (`{npx}`) is not run here.
fn tool_probe_checks(ctx: &Ctx, hosts: &[Box<dyn Host>], checks: &mut Vec<Check>) -> Result<()> {
    let Some(lock) = Lockfile::load(ctx)? else {
        return Ok(());
    };
    let recipes = load_all()?;
    for (id, tool) in &lock.tools {
        let Some(recipe) = recipes.iter().find(|recipe| &recipe.id == id) else {
            continue;
        };
        for host_name in &tool.hosts {
            let Ok(kind) = host_name.parse::<HostKind>() else {
                continue;
            };
            if !hosts.iter().any(|host| host.info().kind == kind) {
                continue;
            }
            let Some(probe) = recipe.probe.get(&kind) else {
                continue;
            };
            let check_id = format!("tool.{id}.{kind}");
            let Some((program, args)) = probe.argv.split_first() else {
                continue;
            };
            let shown = std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            if program.starts_with('{') {
                // Not run is never an ok: the probe is unverified until someone runs it (ruling Important item 6).
                checks.push(Check::warn(
                    &check_id,
                    "not run: its program is set at install time",
                    format!("doctor does not run this probe; run `{shown}` yourself to check it"),
                ));
                continue;
            }
            let Some(path) = probe_program(ctx, program) else {
                checks.push(Check::fail(
                    &check_id,
                    format!("`{shown}` cannot run: {program} is not found on the probe PATH"),
                    "run `relaxkit setup` again",
                ));
                continue;
            };
            let cmd = args
                .iter()
                .fold(Cmd::new(&path.to_string_lossy()), |cmd, arg| cmd.arg(arg))
                .env("PATH", &probe_path(ctx));
            checks.push(match ctx.runner.run(&cmd) {
                Ok(out) if out.code == 0 => Check::ok(&check_id, format!("`{shown}` passed")),
                Ok(out) => Check::fail(
                    &check_id,
                    format!("`{shown}` exited {}", out.code),
                    "run `relaxkit setup` again",
                ),
                Err(err) => Check::fail(
                    &check_id,
                    format!("`{shown}` did not run: {err:#}"),
                    "run `relaxkit setup` again",
                ),
            });
        }
    }
    Ok(())
}

/// Each locked MCP entry's command must resolve on the MCP PATH (spec 6, ruling K58). The command is only looked for on
/// disk: the server is not started and no network is used. A missing command is a fail.
fn mcp_checks(ctx: &Ctx, hosts: &[Box<dyn Host>], checks: &mut Vec<Check>) -> Result<()> {
    let Some(lock) = Lockfile::load(ctx)? else {
        return Ok(());
    };
    let recipes = load_all()?;
    let dirs = mcp_path_dirs(ctx);
    for (id, tool) in &lock.tools {
        let Some(recipe) = recipes
            .iter()
            .find(|recipe| &recipe.id == id && recipe.kind == Kind::Mcp)
        else {
            continue;
        };
        for host_name in &tool.hosts {
            let Ok(kind) = host_name.parse::<HostKind>() else {
                continue;
            };
            let Some(host) = hosts.iter().find(|host| host.info().kind == kind) else {
                continue;
            };
            let entry =
                recipe
                    .install
                    .get(&kind)
                    .into_iter()
                    .flatten()
                    .find_map(|step| match step {
                        Step::Mcp { name, argv } => {
                            argv.first().map(|program| (name.clone(), program.clone()))
                        }
                        _ => None,
                    });
            let Some((entry, program)) = entry else {
                continue;
            };
            checks.push(mcp_entry_check(ctx, host.as_ref(), &entry, id, kind));
            let check_id = format!("mcp.{id}.{kind}");
            let found = match program.as_str() {
                "{npx}" => Some(npx_bin(ctx)).filter(|path| is_executable(path)),
                "{node}" => Some(node_bin(ctx)).filter(|path| is_executable(path)),
                _ => dirs
                    .iter()
                    .map(|dir| dir.join(&program))
                    .find(|path| is_executable(path)),
            };
            checks.push(match found {
                Some(path) => Check::ok(
                    &check_id,
                    format!("`{program}` resolves on the MCP PATH: {}", path.display()),
                ),
                None => Check::fail(
                    &check_id,
                    format!("`{program}` is not on the MCP PATH, so the {kind} entry cannot start"),
                    format!("run `relaxkit setup` again; it reinstalls {id} and re-adds the entry"),
                ),
            });
        }
    }
    Ok(())
}

/// Whether a locked MCP entry is in the host's own config (OWNER-3 (c), ruling R2-1). A missing entry is a fail that
/// setup repairs by adding it again; a config that cannot be read is a fail with the fix to repair it by hand.
fn mcp_entry_check(ctx: &Ctx, host: &dyn Host, entry: &str, id: &str, kind: HostKind) -> Check {
    let check_id = format!("mcp-entry.{id}.{kind}");
    match host.mcp_entry_present(ctx, entry) {
        Ok(true) => Check::ok(&check_id, format!("{entry} is in the {kind} config")),
        Ok(false) => Check::fail(
            &check_id,
            format!("{entry} is missing from the {kind} config"),
            "run relaxkit setup; it re-adds the entry",
        ),
        // An unreadable config has its own id, so setup never treats it as a missing entry and rewrites it (ruling R3-3).
        Err(err) => Check::fail(
            &format!("mcp-config.{id}.{kind}"),
            format!("the {kind} config cannot be read: {err:#}"),
            "relaxkit does not rewrite a file that does not parse; repair it by hand",
        ),
    }
}

/// The file a probe's program runs from, looked up the way the engine looks: the Node link, the pack bin, `~/.local/bin`,
/// the system directories, then the PATH. Doctor runs that file by its full path (ruling Important 6).
fn probe_program(ctx: &Ctx, program: &str) -> Option<PathBuf> {
    let dirs = [
        ctx.dirs.data.join("node").join("bin"),
        ctx.dirs.bin.clone(),
        ctx.dirs.home.join(".local").join("bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|path| is_executable(path))
        .or_else(|| ctx.runner.which(program))
}

/// The JSON document `doctor --json` prints: `{"checks":[{"id","status","message","fix"}]}`.
pub fn to_json(checks: &[Check]) -> Value {
    let entries: Vec<Value> = checks
        .iter()
        .map(|check| {
            json!({
                "id": check.id,
                "status": check.status.as_str(),
                "message": check.message,
                "fix": check.fix,
            })
        })
        .collect();
    json!({ "checks": entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Os, prerequisite_plan};

    #[test]
    fn missing_sandbox_packages_fail_with_the_one_sudo_command_as_the_fix() {
        let plan = prerequisite_plan(Os::Linux, None);

        let check = prerequisites_check(&plan, |_| false);

        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("bwrap and socat"), "{check:?}");
        assert_eq!(
            check.fix.as_deref(),
            Some(
                format!(
                    "run this once yourself (relaxkit does not run sudo without --allow-sudo): {}",
                    sudo_command(&plan)
                )
                .as_str()
            )
        );
        assert!(sudo_command(&plan).starts_with("sudo sh -c"));
    }

    #[test]
    fn present_sandbox_packages_pass() {
        let plan = prerequisite_plan(Os::Linux, None);

        let check = prerequisites_check(&plan, |_| true);

        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn a_plan_with_no_packages_passes_on_macos() {
        let check = prerequisites_check(&prerequisite_plan(Os::MacOs, None), |_| false);

        assert_eq!(check.status, CheckStatus::Ok);
    }

    const UBUNTU_2404: &str = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";

    #[test]
    fn a_missing_apparmor_profile_on_ubuntu_2404_fails_with_the_full_sudo_plan() {
        // bubblewrap and socat are present, but the AppArmor profile is not. The fix prints the whole plan, profile step
        // included, so no user is told to run an incomplete command (ruling K57).
        let plan = prerequisite_plan(Os::Linux, Some(UBUNTU_2404));

        let check = prerequisites_check(&plan, |_| true);

        assert_eq!(check.status, CheckStatus::Fail, "{check:?}");
        let fix = check.fix.clone().unwrap_or_default();
        assert!(fix.starts_with("run this once yourself"), "{check:?}");
        assert!(fix.contains("apparmor_parser"), "{check:?}");
    }
}
