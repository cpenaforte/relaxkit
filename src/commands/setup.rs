//! `relaxkit setup` (spec sections 4, 6 and 7). The steps run in spec order: detect, prerequisites, install,
//! configure, sandbox, login, verify. Each step prints one line. Rulings K46 to K54 in batch-14-brief.md apply.

use std::collections::BTreeSet;

use anyhow::{Result, bail};

use crate::commands::doctor::{self, Check, CheckStatus};
use crate::config::KitConfig;
use crate::config_apply::{claude, opencode, voice};
use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::github::{self, LoginOutcome};
use crate::host::{Host, HostKind, detect_hosts};
use crate::platform::{
    PrereqPlan, apparmor_profile_present, is_executable, plan_needs_apparmor_profile,
};
use crate::recipe::{Recipe, load_all};
use crate::runtime::{gh, nvm, uv};
use crate::sandbox::{self, GhPath};
use crate::state::lockfile::Lockfile;
use crate::state::{RunId, with_run_lock};
use crate::tools::{Engine, Outcome};

/// The steps in the order they run (spec section 6, ruling K46).
pub const STEPS: [&str; 7] = [
    "detect",
    "prerequisites",
    "install",
    "configure",
    "sandbox",
    "login",
    "verify",
];

/// The exit code when the GitHub login needs the owner (ruling K48).
const LOGIN_PENDING_EXIT: i32 = 3;

/// The exit code when a doctor check failed. It takes precedence over the login (ruling K50).
const FAILED_CHECK_EXIT: i32 = 1;

/// One printed line, tagged with its step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLine {
    pub step: &'static str,
    pub line: String,
}

/// What a setup run did. `exit_code` is 0 when all is well, 1 when a doctor check failed, 3 when the owner must
/// finish the GitHub login.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SetupReport {
    pub steps: Vec<StepLine>,
    pub exit_code: i32,
}

/// Runs setup. Nothing is written in a dry run, which only prints the plan.
pub fn run(ctx: &Ctx) -> Result<SetupReport> {
    let hosts = detect_hosts(ctx);
    if hosts.is_empty() {
        bail!(
            "neither Claude Code nor OpenCode was found on the PATH; install one of them first, then run `relaxkit setup` again"
        );
    }
    if ctx.dry_run {
        return Ok(plan_only(ctx, &hosts));
    }
    with_run_lock(&ctx.dirs, || execute(ctx, &hosts))
}

/// The dry-run output: one line per step, nothing run beyond host detection.
fn plan_only(ctx: &Ctx, hosts: &[Box<dyn Host>]) -> SetupReport {
    let mut report = SetupReport::default();
    let found = host_names(hosts);
    report.say(ctx, "detect", format!("found {found}"));
    report.say(
        ctx,
        "prerequisites",
        "would install or adopt Node 24 through nvm, uv and gh".to_string(),
    );
    report.say(
        ctx,
        "install",
        "would install or adopt the pack's tools on each detected host, skipping tools already locked".to_string(),
    );
    report.say(
        ctx,
        "configure",
        "would write the balanced preset, the roles, the telemetry switch and the voice"
            .to_string(),
    );
    report.say(
        ctx,
        "sandbox",
        "would write the sandbox settings; Linux packages are printed for you to run unless --allow-sudo is given".to_string(),
    );
    report.say(
        ctx,
        "login",
        "would log in to GitHub if gh is not logged in".to_string(),
    );
    report.say(
        ctx,
        "verify",
        "would run the probes of every locked tool".to_string(),
    );
    report
}

fn execute(ctx: &Ctx, hosts: &[Box<dyn Host>]) -> Result<SetupReport> {
    let run = ctx.run.clone();
    let mut report = SetupReport::default();
    report.say(ctx, "detect", format!("found {}", host_names(hosts)));

    let node = nvm::ensure_node(ctx, &run)?;
    nvm::link_node(ctx, &node)?;
    uv::ensure_uv(ctx, &run)?;
    gh::ensure_gh(ctx, &run)?;
    report.say(
        ctx,
        "prerequisites",
        format!("Node {} through nvm, uv and gh are in place", node.version),
    );

    let recipes = install_order(load_all()?)?;
    let present: Vec<HostKind> = hosts.iter().map(|host| host.info().kind).collect();
    let locked = Lockfile::load(ctx)?;
    let mut installed = 0;
    let mut kept = 0;
    let engine = Engine {
        ctx,
        run: &run,
        hosts,
        recipes: &recipes,
        node: &node,
    };
    for recipe in &recipes {
        if already_locked(recipe, &present, locked.as_ref()) {
            kept += 1;
            continue;
        }
        let outcomes = engine.install(recipe)?;
        if outcomes
            .iter()
            .any(|(_, outcome)| !matches!(outcome, Outcome::Skipped { .. }))
        {
            installed += 1;
        }
    }
    report.say(
        ctx,
        "install",
        format!("{installed} tools installed or adopted, {kept} already locked"),
    );

    configure(ctx, &run, hosts, &mut report)?;
    sandbox_step(ctx, &run, hosts, &mut report)?;
    let login = login_step(ctx, &run, &mut report)?;
    let failed = verify_step(ctx, hosts, &engine, &mut report)?;

    report.exit_code = match login {
        _ if failed => FAILED_CHECK_EXIT,
        Some(LoginOutcome::ManualStep { .. }) => LOGIN_PENDING_EXIT,
        _ => 0,
    };
    Ok(report)
}

/// Applies the kit's configuration to each detected host. An existing `config.toml` keeps the owner's preset and
/// settings; a fresh machine gets the balanced preset with telemetry off and no compact threshold (ruling K46).
fn configure(
    ctx: &Ctx,
    run: &RunId,
    hosts: &[Box<dyn Host>],
    report: &mut SetupReport,
) -> Result<()> {
    let config = KitConfig::load(&ctx.dirs)?;
    config.save(ctx, run)?;
    let voice_word = config.voice.unwrap_or_else(|| config.preset.voice());
    voice::apply(ctx, run, voice_word, false)?;
    for host in hosts {
        match host.info().kind {
            HostKind::Claude => {
                claude::apply_roles(ctx, run, host.as_ref(), &config)?;
                claude::apply_privacy_env(ctx, run, host.as_ref())?;
            }
            HostKind::Opencode => {
                let available = opencode_models(ctx, host.as_ref());
                opencode::apply_roles(ctx, run, host.as_ref(), &config, &available)?;
            }
        }
    }
    report.say(
        ctx,
        "configure",
        format!(
            "preset {} with voice {}, telemetry off where relaxkit can set it; no compact threshold",
            config.preset.as_str(),
            voice_word.as_str()
        ),
    );
    Ok(())
}

/// The OpenCode models the roles can map onto. A host that cannot list them yields none, so each agent inherits
/// the host's default model.
pub fn opencode_models(ctx: &Ctx, host: &dyn Host) -> Vec<String> {
    let binary = host.info().binary.to_string_lossy().into_owned();
    match ctx.runner.run(&Cmd::new(&binary).arg("models")) {
        Ok(out) if out.code == 0 => out
            .stdout
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The sandbox of each host (spec section 7). Claude Code always takes `gh` outside its sandbox (ruling K42).
/// Linux packages need sudo: they run only with `--allow-sudo`, otherwise the command is printed (ruling K47).
fn sandbox_step(
    ctx: &Ctx,
    run: &RunId,
    hosts: &[Box<dyn Host>],
    report: &mut SetupReport,
) -> Result<()> {
    let plan = sandbox::linux_prerequisites(ctx)?;
    let packages = sudo_step(ctx, &plan)?;
    let locked = Lockfile::load(ctx)?;
    let mut notes = Vec::new();
    for host in hosts {
        match host.info().kind {
            HostKind::Claude => {
                let gh = github::choose_gh_path(ctx, host.as_ref());
                debug_assert_eq!(gh, GhPath::ExcludedFallback);
                sandbox::claude::apply(ctx, run, host.as_ref(), gh)?;
                notes.push("Claude Code sandbox written".to_string());
            }
            HostKind::Opencode => {
                let version = locked
                    .as_ref()
                    .and_then(|lock| lock.tools.get("opencode-sandbox"))
                    .filter(|tool| tool.hosts.iter().any(|h| h == "opencode"))
                    .map(|tool| tool.version.clone());
                match version {
                    Some(version) => {
                        sandbox::opencode::apply(ctx, run, host.as_ref(), &version)?;
                        notes.push(format!("OpenCode sandbox {version} written"));
                    }
                    None => notes
                        .push("OpenCode sandbox skipped: its plugin is not installed".to_string()),
                }
            }
        }
    }
    notes.push(packages);
    report.say(ctx, "sandbox", notes.join("; "));
    Ok(())
}

/// The sudo plan for the Linux packages. Returns what happened, as one line. Nothing runs without `--allow-sudo`,
/// and nothing runs when bubblewrap and socat are already on the PATH.
fn sudo_step(ctx: &Ctx, plan: &PrereqPlan) -> Result<String> {
    if plan.sudo.is_empty() {
        return Ok("no Linux packages needed".to_string());
    }
    let have = ["bwrap", "socat"]
        .iter()
        .all(|program| ctx.runner.which(program).is_some());
    // On Ubuntu 24.04 and later the AppArmor profile is part of the same sudo plan, so it must be on disk too (ruling K57).
    let profile_ready = !plan_needs_apparmor_profile(plan) || apparmor_profile_present();
    if have && profile_ready {
        return Ok("bubblewrap and socat are already installed".to_string());
    }
    if ctx.allow_sudo {
        sandbox::run_prerequisites(ctx, plan, true)?;
        return Ok("Linux packages installed with one sudo run".to_string());
    }
    Ok(format!(
        "Linux packages are missing; run this once yourself, then run setup again: {}",
        sudo_command(plan)
    ))
}

/// The one sudo invocation of a plan, shell-quoted for display.
pub fn sudo_command(plan: &PrereqPlan) -> String {
    match plan.sudo.split_first() {
        Some((program, args)) => args
            .iter()
            .fold(Cmd::new(program), |cmd, arg| cmd.arg(arg))
            .to_string(),
        None => String::new(),
    }
}

/// Runs the GitHub login when gh is present (ruling K48). Returns the outcome, or `None` when the step was skipped.
fn login_step(ctx: &Ctx, run: &RunId, report: &mut SetupReport) -> Result<Option<LoginOutcome>> {
    let present = ctx.runner.which("gh").is_some() || is_executable(&ctx.dirs.bin.join("gh"));
    if !present {
        report.say(ctx, "login", "skipped: gh is not installed".to_string());
        return Ok(None);
    }
    let outcome = github::login(ctx, run)?;
    let line = match &outcome {
        LoginOutcome::AlreadyAuthenticated => "already logged in to GitHub".to_string(),
        LoginOutcome::LoggedIn => "logged in to GitHub".to_string(),
        LoginOutcome::Planned => "would log in to GitHub".to_string(),
        LoginOutcome::ManualStep { command } => {
            format!("finish the login yourself, then run setup again: {command}")
        }
    };
    report.say(ctx, "login", line);
    Ok(Some(outcome))
}

/// The verify step runs `doctor` (ruling K50). A failing probe or MCP command is repaired by reinstalling its locked
/// tool, and the checks run again (ruling OWNER-3). The checks print here, and a check that still fails makes setup
/// exit 1. Returns whether any check failed.
fn verify_step(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    engine: &Engine<'_>,
    report: &mut SetupReport,
) -> Result<bool> {
    let mut checks = doctor::checks_for(ctx, hosts)?;
    let mut repairs: Vec<Repair> = Vec::new();
    for check in &checks {
        if let Some(repair) = repair_for(check)
            && !repairs.contains(&repair)
        {
            repairs.push(repair);
        }
    }
    // The locked versions as they were before the repairs, for the repair lines (a repair rewrites the lock).
    let locked_before = Lockfile::load(ctx)?;
    let locked_version = |id: &str| {
        locked_before
            .as_ref()
            .and_then(|lock| lock.tools.get(id))
            .map(|tool| tool.version.clone())
            .unwrap_or_else(|| "unlocked".to_string())
    };
    for repair in &repairs {
        match repair {
            Repair::Reinstall(id, kind) => {
                let Some(recipe) = engine.recipes.iter().find(|recipe| &recipe.id == id) else {
                    continue;
                };
                let locked = locked_version(id);
                match engine.reinstall(recipe, *kind) {
                    Ok(Outcome::Installed { version }) if id == "rtk" => report.say(
                        ctx,
                        "verify",
                        format!("repaired {id} on {kind}: installed the latest release {version}"),
                    ),
                    Ok(Outcome::Installed { version }) => report.say(
                        ctx,
                        "verify",
                        format!("repaired {id} on {kind}: installed {version}, locked at {locked}"),
                    ),
                    Ok(Outcome::Adopted { version, .. }) => report.say(
                        ctx,
                        "verify",
                        format!(
                            "repaired {id} on {kind}: {version} is already installed, so only its configuration ran again (locked at {locked})"
                        ),
                    ),
                    Ok(Outcome::Skipped { reason }) => report.say(
                        ctx,
                        "verify",
                        format!("could not repair {id} on {kind}: {reason}"),
                    ),
                    Err(err) => report.say(
                        ctx,
                        "verify",
                        format!("could not repair {id} on {kind}: {err:#}"),
                    ),
                }
            }
            Repair::ReAddEntry(id, kind) => {
                let Some(recipe) = engine.recipes.iter().find(|recipe| &recipe.id == id) else {
                    continue;
                };
                match engine.readd_mcp(recipe, *kind) {
                    Ok(()) => report.say(
                        ctx,
                        "verify",
                        format!("repaired {id} on {kind}: re-added its MCP entry"),
                    ),
                    Err(err) => report.say(
                        ctx,
                        "verify",
                        format!("could not repair {id} on {kind}: {err:#}"),
                    ),
                }
            }
        }
    }
    if !repairs.is_empty() {
        checks = doctor::checks_for(ctx, hosts)?;
    }
    let count = |status: CheckStatus| checks.iter().filter(|check| check.status == status).count();
    let (ok, warn, fail) = (
        count(CheckStatus::Ok),
        count(CheckStatus::Warn),
        count(CheckStatus::Fail),
    );
    for check in checks
        .iter()
        .filter(|check| check.status != CheckStatus::Ok)
    {
        let fix = check
            .fix
            .as_deref()
            .map(|fix| format!("; fix: {fix}"))
            .unwrap_or_default();
        report.say(
            ctx,
            "verify",
            format!(
                "{} {}: {}{fix}",
                check.status.as_str(),
                check.id,
                check.message
            ),
        );
    }
    // The summary is the last line of the step.
    report.say(
        ctx,
        "verify",
        format!(
            "doctor ran {} checks: {ok} ok, {warn} warn, {fail} fail",
            checks.len()
        ),
    );
    Ok(fail > 0)
}

/// A repair a failing check gets on a rerun (ruling OWNER-3, and R2-1 for the MCP entry).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Repair {
    /// Reinstall the locked tool on the host, through the engine path.
    Reinstall(String, HostKind),
    /// Add the locked MCP entry again through the host's MCP step, without reinstalling the tool.
    ReAddEntry(String, HostKind),
}

/// The repair for a failing check, or `None` when no repair exists. A failing probe (`tool.<id>.<host>`) or MCP
/// command (`mcp.<id>.<host>`) reinstalls the locked tool; a missing MCP entry (`mcp-entry.<id>.<host>`) is re-added.
fn repair_for(check: &Check) -> Option<Repair> {
    if check.status != CheckStatus::Fail {
        return None;
    }
    if let Some(rest) = check.id.strip_prefix("mcp-entry.") {
        let (id, kind) = rest.rsplit_once('.')?;
        return Some(Repair::ReAddEntry(id.to_string(), kind.parse().ok()?));
    }
    let rest = check
        .id
        .strip_prefix("tool.")
        .or_else(|| check.id.strip_prefix("mcp."))?;
    let (id, kind) = rest.rsplit_once('.')?;
    Some(Repair::Reinstall(id.to_string(), kind.parse().ok()?))
}

/// A recipe is left alone when every host it names that is installed already has it in the lock. The lock
/// records what setup installed; `update` is what moves versions, so setup does not reinstall (ruling K52).
fn already_locked(recipe: &Recipe, present: &[HostKind], lock: Option<&Lockfile>) -> bool {
    let Some(entry) = lock.and_then(|lock| lock.tools.get(&recipe.id)) else {
        return false;
    };
    let installed: Vec<HostKind> = recipe
        .hosts
        .iter()
        .copied()
        .filter(|kind| present.contains(kind))
        .collect();
    !installed.is_empty()
        && installed
            .iter()
            .all(|kind| entry.hosts.iter().any(|host| host == kind.as_str()))
}

/// The recipes in an order where each one comes after the recipes it requires (for example playwright before
/// chrome-devtools).
fn install_order(recipes: Vec<Recipe>) -> Result<Vec<Recipe>> {
    let ids: BTreeSet<String> = recipes.iter().map(|recipe| recipe.id.clone()).collect();
    let mut pending = recipes;
    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let (ready, waiting): (Vec<Recipe>, Vec<Recipe>) =
            pending.into_iter().partition(|recipe| {
                recipe
                    .requires
                    .iter()
                    .all(|id| placed.contains(id) || !ids.contains(id))
            });
        if ready.is_empty() {
            let names: Vec<&str> = waiting.iter().map(|recipe| recipe.id.as_str()).collect();
            bail!(
                "recipes require each other in a cycle: {}",
                names.join(", ")
            );
        }
        placed.extend(ready.iter().map(|recipe| recipe.id.clone()));
        ordered.extend(ready);
        pending = waiting;
    }
    Ok(ordered)
}

fn host_names(hosts: &[Box<dyn Host>]) -> String {
    hosts
        .iter()
        .map(|host| {
            let info = host.info();
            format!("{} {}", info.kind.product_name(), info.version)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl SetupReport {
    /// Records a step's line and prints it as `step: line`.
    fn say(&mut self, ctx: &Ctx, step: &'static str, line: String) {
        ctx.out.info(&format!("{step}: {line}"));
        self.steps.push(StepLine { step, line });
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::platform::{Os, prerequisite_plan};

    fn linux_plan() -> PrereqPlan {
        prerequisite_plan(Os::Linux, None)
    }

    #[test]
    fn sudo_plan_is_printed_and_not_run_without_allow_sudo() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());

        let line = sudo_step(&ctx, &linux_plan()).unwrap();

        assert!(line.starts_with("Linux packages are missing"), "{line}");
        assert!(line.contains("sudo sh -c"), "{line}");
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn sudo_plan_runs_once_with_allow_sudo() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("sudo", Output::ok(""));
        let mut ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());
        ctx.allow_sudo = true;

        sudo_step(&ctx, &linux_plan()).unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("sudo sh -c"), "{calls:?}");
    }

    #[test]
    fn sudo_plan_is_skipped_when_bubblewrap_and_socat_are_present() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new()
            .with_program("bwrap", Path::new("/usr/bin/bwrap"))
            .with_program("socat", Path::new("/usr/bin/socat"));
        let mut ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());
        ctx.allow_sudo = true;

        let line = sudo_step(&ctx, &linux_plan()).unwrap();

        assert!(line.contains("already installed"), "{line}");
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn an_empty_sudo_plan_needs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());

        let line = sudo_step(&ctx, &PrereqPlan::default()).unwrap();

        assert_eq!(line, "no Linux packages needed");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_recipe_waits_for_the_recipe_it_requires() {
        let mut recipes = crate::recipe::load_all().unwrap();
        recipes.reverse();

        let ordered = install_order(recipes).unwrap();

        let position = |id: &str| ordered.iter().position(|r| r.id == id).unwrap();
        assert!(position("playwright") < position("chrome-devtools"));
    }

    const UBUNTU_2404: &str = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";

    #[test]
    fn sudo_plan_runs_on_ubuntu_2404_when_the_apparmor_profile_is_missing() {
        // bubblewrap and socat are present, but Ubuntu 24.04 also needs the AppArmor profile, which is part of the same
        // sudo plan (spec 7; ruling K57). The plan runs with --allow-sudo and is only printed without it.
        let root = tempfile::tempdir().unwrap();
        let plan = prerequisite_plan(Os::Linux, Some(UBUNTU_2404));
        let programs = |fake: FakeRunner| {
            fake.with_program("bwrap", Path::new("/usr/bin/bwrap"))
                .with_program("socat", Path::new("/usr/bin/socat"))
        };
        let fake = programs(FakeRunner::new().reply("sudo", Output::ok("")));
        let mut ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());
        ctx.allow_sudo = true;

        let line = sudo_step(&ctx, &plan).unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{line}");
        assert!(calls[0].contains("apparmor_parser"), "{calls:?}");

        let fake = programs(FakeRunner::new());
        let ctx = Ctx::for_test(root.path(), fake.clone(), FakeNet::new());
        let line = sudo_step(&ctx, &plan).unwrap();
        assert!(line.contains("apparmor_parser"), "{line}");
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }
}
