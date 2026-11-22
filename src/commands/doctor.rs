//! `relaxkit doctor` (spec sections 5 and 7, 10 items 11 and 15, 25): the checks that decide whether the pack is
//! sound. Read-only: nothing here rewrites a file. Each check has a stable id, its outcome, what it found, and a fix
//! when it is not ok (rulings K47, K50, K53).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::adapters::OPENCODE_PLUGIN;
use crate::commands::read_optional;
use crate::commands::setup::sudo_command;
use crate::config_apply::caveman::PLUGIN_SPEC;
use crate::ctx::Ctx;
use crate::edit::jsonc_get;
use crate::exec::Cmd;
use crate::host::{Host, HostKind, detect_hosts, v1_mcp_entries};
use crate::platform::{
    BWRAP_PROFILE_PATH, PrereqPlan, apparmor_profile_present, is_executable,
    plan_needs_apparmor_profile,
};
use crate::recipe::{Kind, Probe, Recipe, Step, load_all};
use crate::runtime::nvm::{node_bin, npx_bin};
use crate::sandbox::health::{health_line, is_stale, plugin_health};
use crate::sandbox::linux_prerequisites;
use crate::sandbox::selftest::selftest;
use crate::state::lockfile::Lockfile;
use crate::tools::detect::{plugin_name, plugin_spec};
use crate::tools::engine::{mcp_path_dirs, probe_path};
use crate::ui::{Mark, Palette, heading, key_values, outcome};

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

    /// The mark a line with this outcome starts with.
    pub fn mark(self) -> Mark {
        match self {
            CheckStatus::Ok => Mark::Ok,
            CheckStatus::Warn => Mark::Warn,
            CheckStatus::Fail => Mark::Fail,
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
    plugin_check(ctx, hosts, &mut checks)?;
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

/// The OpenCode sandbox plugin (spec 7; spec 10 item 25). The plugin must be listed, or the OpenCode sandbox is not
/// enforced. The check warns only when relaxkit has had no release for 120 days; a runtime lag never warns on its own,
/// and an unreachable registry or GitHub is not a failure.
fn plugin_check(ctx: &Ctx, hosts: &[Box<dyn Host>], checks: &mut Vec<Check>) -> Result<()> {
    let Some(host) = hosts
        .iter()
        .find(|host| host.info().kind == HostKind::Opencode)
    else {
        return Ok(());
    };
    let id = "opencode.sandbox.plugin";
    let listed = match host.plugin_list(ctx) {
        Ok(plugins) => plugins.iter().any(|plugin| plugin.name == OPENCODE_PLUGIN),
        Err(err) => {
            checks.push(Check::fail(
                id,
                format!("the OpenCode plugins entry cannot be read: {err:#}"),
                "repair the plugins entry by hand; relaxkit does not rewrite a file it cannot read",
            ));
            return Ok(());
        }
    };
    if !listed {
        checks.push(Check::fail(
            id,
            format!(
                "{OPENCODE_PLUGIN} is not in the OpenCode plugins list, so the OpenCode sandbox is not enforced"
            ),
            "run relaxkit setup",
        ));
        return Ok(());
    }
    let health = plugin_health(ctx);
    let line = format!("{OPENCODE_PLUGIN} is listed; {}", health_line(&health));
    if is_stale(&health) {
        checks.push(Check::warn(
            id,
            line,
            "relaxkit has had no release for 120 days; run relaxkit update --self when a newer one is out",
        ));
    } else {
        checks.push(Check::ok(id, line));
    }
    Ok(())
}

/// The command a person runs to check a probe `doctor` does not run, with the locked version filled in.
fn shown_argv(argv: &[String], version: &str) -> String {
    argv.iter()
        .map(|arg| arg.replace("{npx}", "npx").replace("{version}", version))
        .collect::<Vec<_>>()
        .join(" ")
}

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
            let Some(host) = hosts.iter().find(|host| host.info().kind == kind) else {
                continue;
            };
            let Some(probe) = recipe.probe.get(&kind) else {
                continue;
            };
            let check_id = format!("tool.{id}.{kind}");
            let Some((program, args)) = probe.argv.split_first() else {
                continue;
            };
            let shown = shown_argv(&probe.argv, &tool.version);
            if program.starts_with('{') {
                // Not run is never an ok: the probe is unverified until someone runs it (ruling Important item 6).
                checks.push(Check::warn(
                    &check_id,
                    "not run here: the program is chosen at install time",
                    format!("run `{shown}` to check it yourself"),
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
            let expected = expected_text(recipe, kind, probe);
            checks.push(match ctx.runner.run(&cmd) {
                Ok(out) if out.code == 0 => match expected {
                    // A probe passes only when its output holds the expected text: exit 0 alone is not enough (R3-5).
                    Some(text) if !out.stdout.contains(&text) && !out.stderr.contains(&text) => {
                        Check::fail(
                            &check_id,
                            format!("`{shown}` exited 0 but its output does not contain {text}"),
                            "run `relaxkit setup` again",
                        )
                    }
                    // A plugin probe also needs the plugin listed by the host's config (R4-2).
                    _ => unlisted_plugin(ctx, host.as_ref(), recipe, kind, &check_id)
                        .unwrap_or_else(|| Check::ok(&check_id, format!("`{shown}` passed"))),
                },
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

/// The text a passing probe's output must hold: the recipe's `expect`, or, for a `plugin list` probe with none, the
/// recipe's plugin name for this host (ruling R3-5).
fn expected_text(recipe: &Recipe, kind: HostKind, probe: &Probe) -> Option<String> {
    if let Some(expect) = &probe.expect {
        return Some(expect.clone());
    }
    let lists_plugins =
        probe.argv.iter().any(|arg| arg == "plugin") && probe.argv.iter().any(|arg| arg == "list");
    if lists_plugins {
        return plugin_spec(recipe, kind).map(|spec| plugin_name(spec).to_string());
    }
    None
}

/// The check for a recipe's plugin on this host when the probe passed, or `None` when the plugin is listed. A recipe with
/// no plugin for the host has no check here (ruling R4-2). An OpenCode `plugins` entry that cannot be read gets its own
/// id, which setup never repairs (ruling R5-1).
fn unlisted_plugin(
    ctx: &Ctx,
    host: &dyn Host,
    recipe: &Recipe,
    kind: HostKind,
    check_id: &str,
) -> Option<Check> {
    let name = match plugin_spec(recipe, kind) {
        Some(spec) => plugin_name(spec).to_string(),
        // caveman's OpenCode entry is the path its config step writes, not a recipe plugin spec (ruling R5-2).
        None if recipe.id == "caveman" && kind == HostKind::Opencode => {
            plugin_name(PLUGIN_SPEC).to_string()
        }
        None => return None,
    };
    match host.plugin_list(ctx) {
        Ok(plugins) if plugins.iter().any(|plugin| plugin.name == name) => None,
        Ok(_) => Some(Check::fail(
            check_id,
            format!("{name} is not listed in the {kind} config"),
            "run relaxkit setup",
        )),
        Err(err) if kind == HostKind::Opencode => Some(Check::fail(
            &format!("plugin-config.{}.{kind}", recipe.id),
            format!("the {kind} plugins entry cannot be read: {err:#}"),
            "repair the plugins entry by hand; relaxkit does not rewrite a file it cannot read",
        )),
        Err(err) => Some(Check::fail(
            check_id,
            format!("the {kind} plugin list cannot be read: {err:#}"),
            "run relaxkit setup",
        )),
    }
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

/// The areas the text output groups checks under, in the order they print.
const AREAS: [&str; 6] = [
    "Sandbox",
    "Claude Code",
    "OpenCode",
    "Tools",
    "MCP",
    "Other",
];

/// The area a check is listed under, from its id.
fn area(id: &str) -> &'static str {
    if id.starts_with("sandbox.") {
        "Sandbox"
    } else if id.starts_with("claude.") {
        "Claude Code"
    } else if id.starts_with("opencode.") {
        "OpenCode"
    } else if id.starts_with("tool.") || id.starts_with("plugin-config.") {
        "Tools"
    } else if id.starts_with("mcp") {
        "MCP"
    } else {
        "Other"
    }
}

/// Non-ok checks that say the same thing: one finding per outcome and message, in the order they first appear.
pub struct Finding<'a> {
    pub status: CheckStatus,
    pub message: &'a str,
    pub checks: Vec<&'a Check>,
}

/// The findings of `checks`. Ok checks are left out.
pub fn findings(checks: &[Check]) -> Vec<Finding<'_>> {
    let mut found: Vec<Finding<'_>> = Vec::new();
    for check in checks
        .iter()
        .filter(|check| check.status != CheckStatus::Ok)
    {
        match found
            .iter_mut()
            .find(|finding| finding.status == check.status && finding.message == check.message)
        {
            Some(finding) => finding.checks.push(check),
            None => found.push(Finding {
                status: check.status,
                message: &check.message,
                checks: vec![check],
            }),
        }
    }
    found
}

/// The lines of one finding. A single check shows its id, message and fix. A group shows its message once and the
/// checks under it: with their own fixes when `detail` is set, else with their ids only.
pub fn finding_lines(finding: &Finding<'_>, detail: bool, palette: Palette) -> Vec<String> {
    let status = finding.status.mark();
    if let [check] = finding.checks.as_slice() {
        let mut lines = vec![
            crate::ui::detail(palette, status, &check.id),
            format!("     {}", finding.message),
        ];
        if let Some(fix) = &check.fix {
            lines.push(format!("     fix  {fix}"));
        }
        return lines;
    }
    let mut lines = vec![crate::ui::detail(
        palette,
        status,
        &format!("{}  ({} checks)", finding.message, finding.checks.len()),
    )];
    if detail {
        let rows: Vec<(&str, Vec<String>)> = finding
            .checks
            .iter()
            .map(|check| {
                (
                    check.id.as_str(),
                    vec![check.fix.clone().unwrap_or_default()],
                )
            })
            .collect();
        lines.extend(key_values(&rows, 5));
    } else {
        let ids: Vec<&str> = finding
            .checks
            .iter()
            .map(|check| check.id.as_str())
            .collect();
        lines.push(format!("     {}", ids.join(", ")));
        lines.push("     relaxkit doctor shows the command for each".to_string());
    }
    lines
}

/// The counts of a set of outcomes, as text: `2 failed, 7 warnings, 27 ok`. A count of zero is left out.
pub fn tally_text(fail: usize, warn: usize, ok: usize) -> String {
    let mut parts = Vec::new();
    if fail > 0 {
        parts.push(format!("{fail} failed"));
    }
    if warn > 0 {
        parts.push(format!(
            "{warn} {}",
            if warn == 1 { "warning" } else { "warnings" }
        ));
    }
    if ok > 0 {
        parts.push(format!("{ok} ok"));
    }
    if parts.is_empty() {
        return "no checks".to_string();
    }
    parts.join(", ")
}

/// The outcome counts of `checks`: failures, warnings and ok checks.
fn counts<'a>(checks: impl Iterator<Item = &'a Check>) -> (usize, usize, usize) {
    checks.fold((0, 0, 0), |(fail, warn, ok), check| match check.status {
        CheckStatus::Fail => (fail + 1, warn, ok),
        CheckStatus::Warn => (fail, warn + 1, ok),
        CheckStatus::Ok => (fail, warn, ok + 1),
    })
}

/// The text `doctor` prints: a verdict line, then the checks grouped by area. Ok checks are listed only with `all`;
/// otherwise a footnote says how many are hidden.
pub fn lines(checks: &[Check], all: bool, palette: Palette) -> Vec<String> {
    let (fail, warn, ok) = counts(checks.iter());
    let verdict_mark = if fail > 0 {
        Mark::Fail
    } else if warn > 0 {
        Mark::Warn
    } else {
        Mark::Ok
    };
    let mut lines = vec![outcome(palette, verdict_mark, &tally_text(fail, warn, ok))];
    for name in AREAS {
        let in_area: Vec<&Check> = checks
            .iter()
            .filter(|check| area(&check.id) == name)
            .collect();
        let (area_fail, area_warn, area_ok) = counts(in_area.iter().copied());
        if in_area.is_empty() || (!all && area_fail + area_warn == 0) {
            continue;
        }
        lines.push(String::new());
        lines.push(format!(
            "{}  {}",
            heading(name, palette),
            tally_text(area_fail, area_warn, area_ok)
        ));
        if all {
            for check in in_area
                .iter()
                .filter(|check| check.status == CheckStatus::Ok)
            {
                lines.push(crate::ui::detail(
                    palette,
                    Mark::Ok,
                    &format!("{}  {}", check.id, check.message),
                ));
            }
        }
        let area_checks: Vec<Check> = in_area.iter().map(|check| (*check).clone()).collect();
        for finding in findings(&area_checks) {
            lines.extend(finding_lines(&finding, true, palette));
        }
    }
    if !all && ok > 0 {
        lines.push(String::new());
        lines.push(palette.paint(
            &format!("{ok} ok checks hidden; relaxkit doctor --all lists them"),
            crate::ui::Style::Dim,
        ));
    }
    lines
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
    use crate::ui::Palette;

    #[test]
    fn a_probe_shown_to_a_person_has_its_placeholders_filled_in() {
        let argv = ["{npx}", "-y", "ccusage@{version}", "--offline", "--help"].map(String::from);

        assert_eq!(
            shown_argv(&argv, "20.0.51"),
            "npx -y ccusage@20.0.51 --offline --help"
        );
    }

    fn check(id: &str, status: CheckStatus, message: &str, fix: Option<&str>) -> Check {
        Check {
            id: id.to_string(),
            status,
            message: message.to_string(),
            fix: fix.map(str::to_string),
        }
    }

    #[test]
    fn the_doctor_text_leads_with_a_verdict_and_hides_ok_checks_by_default() {
        let checks = vec![
            check("claude.hooks", CheckStatus::Ok, "hooks are in place", None),
            check(
                "claude.sandbox.selftest",
                CheckStatus::Fail,
                "Claude Code: the sandbox did not refuse",
                Some("run `relaxkit setup` again"),
            ),
        ];

        let lines = lines(&checks, false, Palette::PLAIN);

        assert_eq!(lines[0], "✗  1 failed, 1 ok", "{lines:?}");
        assert!(
            lines.contains(&"Claude Code  1 failed, 1 ok".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  ✗  claude.sandbox.selftest".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"     fix  run `relaxkit setup` again".to_string()),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("claude.hooks")),
            "{lines:?}"
        );
        assert_eq!(
            lines.last().map(String::as_str),
            Some("1 ok checks hidden; relaxkit doctor --all lists them"),
            "{lines:?}"
        );
    }

    #[test]
    fn all_lists_the_ok_checks_too() {
        let checks = vec![check(
            "claude.hooks",
            CheckStatus::Ok,
            "hooks are in place",
            None,
        )];

        let lines = lines(&checks, true, Palette::PLAIN);

        assert!(
            lines.contains(&"  ✓  claude.hooks  hooks are in place".to_string()),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("hidden")),
            "{lines:?}"
        );
    }

    #[test]
    fn identical_findings_are_one_line_with_each_check_and_its_own_fix() {
        let checks = vec![
            check(
                "tool.ccusage.claude",
                CheckStatus::Warn,
                "not run here",
                Some("run `a`"),
            ),
            check(
                "tool.ccusage.opencode",
                CheckStatus::Warn,
                "not run here",
                Some("run `bb`"),
            ),
        ];

        let lines = lines(&checks, false, Palette::PLAIN);

        assert!(
            lines.contains(&"  !  not run here  (2 checks)".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"     tool.ccusage.claude    run `a`".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"     tool.ccusage.opencode  run `bb`".to_string()),
            "{lines:?}"
        );
        assert!(lines[0].starts_with("!  2 warnings"), "{lines:?}");
    }

    #[test]
    fn findings_keep_the_order_they_first_appear_in() {
        let checks = vec![
            check("a", CheckStatus::Warn, "same", None),
            check("b", CheckStatus::Fail, "other", None),
            check("c", CheckStatus::Warn, "same", None),
        ];

        let found = findings(&checks);

        let shape: Vec<(CheckStatus, usize)> = found
            .iter()
            .map(|finding| (finding.status, finding.checks.len()))
            .collect();
        assert_eq!(shape, vec![(CheckStatus::Warn, 2), (CheckStatus::Fail, 1)]);
    }

    #[test]
    fn a_check_lands_under_the_area_its_id_names() {
        assert_eq!(area("sandbox.prerequisites"), "Sandbox");
        assert_eq!(area("claude.sandbox.selftest"), "Claude Code");
        assert_eq!(area("opencode.sandbox.plugin"), "OpenCode");
        assert_eq!(area("tool.ccusage.claude"), "Tools");
        assert_eq!(area("plugin-config.x.opencode"), "Tools");
        assert_eq!(area("mcp-entry.x.claude"), "MCP");
        assert_eq!(area("something-else"), "Other");
    }

    #[test]
    fn tally_text_counts_each_outcome_and_skips_zeros() {
        assert_eq!(tally_text(2, 7, 27), "2 failed, 7 warnings, 27 ok");
        assert_eq!(tally_text(0, 1, 0), "1 warning");
        assert_eq!(tally_text(0, 0, 0), "no checks");
    }

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
