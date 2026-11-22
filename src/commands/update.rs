//! `relaxkit update` for the tools (Task 21, rulings K63 to K82). Self-update is a later step (Task 22), so this module
//! runs the tool phase only. A check changes nothing. A run backs up what it will touch, moves each tool that has a
//! newer release by its own route, proves it with the tool's probe, and returns a tool whose probe fails to its locked
//! version. Every tool is independent: one failure never stops the others (K67, K80).

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::fsx::write_atomic;
use crate::host::{Host, HostCheck, HostKind, check_minimum, detect_hosts};
use crate::net::Net;
use crate::recipe::{Recipe, VersionSource, load_all};
use crate::runtime::nvm::{
    NodeInstall, NvmSearch, current_node, latest_node_24, link_node, update_node_in,
};
use crate::sandbox::candidate_runtime;
use crate::sandbox::opencode::sandbox_config_path;
use crate::sandbox::selftest::selftest_with;
use crate::state::lockfile::{LockedTool, Lockfile};
use crate::state::{record_backup, record_link};
use crate::tools::engine::{Engine, UNVERSIONED};

/// The rows in run order (K80): rtk first, the pinned tools and Node before the tools that run on Node, then the rest.
const ORDER: [&str; 14] = [
    "rtk",
    "nvm",
    "uv",
    "node",
    "superpowers",
    "caveman",
    "playwright",
    "chrome-devtools",
    "serena",
    "ccusage",
    "claude-md-management",
    "frontend-design",
    "opencode-sandbox",
    "gh",
];

/// What `update` was asked to do. A check changes nothing; a tool name limits the run to that row.
#[derive(Debug, Clone, Default)]
pub struct UpdateArgs {
    pub check: bool,
    pub tool: Option<String>,
}

/// What happened to one tool (K75).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowResult {
    /// Moved to the newest release and confirmed by its probe.
    Updated,
    /// Already at the newest release; nothing was run.
    Current,
    /// Its update or probe failed, and it was returned to the locked version.
    RolledBack,
    /// Its update or probe failed, and the locked version could not be restored. The reason is named.
    RollbackFailed(String),
    /// Not changed, with the reason.
    NotTouched(String),
    /// The newest release could not be looked up, so nothing was decided for this tool.
    NewestUnknown,
    /// A check found a newer release. Only a check reports this; a run would move the tool.
    Available,
}

impl RowResult {
    /// The result in words, for the text and JSON output.
    pub fn label(&self) -> String {
        match self {
            RowResult::Updated => "updated".to_string(),
            RowResult::Current => "current".to_string(),
            RowResult::RolledBack => "rolled back".to_string(),
            RowResult::RollbackFailed(reason) => format!("rollback failed: {reason}"),
            RowResult::NotTouched(reason) => format!("not touched: {reason}"),
            RowResult::NewestUnknown => "newest unknown".to_string(),
            RowResult::Available => "update available".to_string(),
        }
    }
}

/// One row of the report: the tool, the version the lock holds, the newest release, and the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRow {
    pub tool: String,
    pub installed: Option<String>,
    pub newest: Option<String>,
    pub result: RowResult,
    /// A fact about the row that a result does not carry, such as a brew copy that was left alone.
    pub note: Option<String>,
}

/// The whole run. `restart` lists the hosts whose tools changed (K69). `failed` names each tool that failed (K75).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub rows: Vec<UpdateRow>,
    pub restart: Vec<HostKind>,
    pub failed: Vec<String>,
}

/// The `update` command for the detected hosts. Used by the CLI.
pub fn run(ctx: &Ctx, args: &UpdateArgs, search: &NvmSearch) -> Result<UpdateReport> {
    let hosts = detect_hosts(ctx);
    run_with_hosts(ctx, args, &hosts, search)
}

/// What the plan decided for one row before any change.
struct Plan<'r> {
    id: &'static str,
    recipe: Option<&'r Recipe>,
    installed: Option<String>,
    newest: Option<String>,
    /// The tool has no version source (a Claude Code plugin), so its newest release is known only after its host
    /// refreshes its marketplace.
    no_source: bool,
    /// A newer release exists (or the tool has no source and must be asked).
    needs: bool,
    /// Why nothing is done, when nothing is.
    skip: Option<RowResult>,
    note: Option<String>,
}

/// The `update` command over the given hosts. Tests call this with scripted hosts; `run` detects them.
pub fn run_with_hosts(
    ctx: &Ctx,
    args: &UpdateArgs,
    hosts: &[Box<dyn Host>],
    search: &NvmSearch,
) -> Result<UpdateReport> {
    if let Some(tool) = &args.tool
        && !ORDER.contains(&tool.as_str())
    {
        bail!("no tool named `{tool}` in the pack");
    }
    let recipes = load_all()?;
    let lock = Lockfile::load(ctx)?;
    let node_now = current_node(ctx);
    let node = node_now.clone().unwrap_or_else(|| NodeInstall {
        version: String::new(),
        root: ctx.dirs.data.join("node"),
    });
    let engine = Engine {
        ctx,
        run: &ctx.run,
        hosts,
        recipes: &recipes,
        node: &node,
    };
    let net: &dyn Net = ctx.net.as_ref();
    let mut plans = Vec::new();
    for id in ORDER {
        if args.tool.as_deref().is_some_and(|tool| tool != id) {
            continue;
        }
        plans.push(plan(
            &engine,
            &recipes,
            lock.as_ref(),
            node_now.as_ref(),
            hosts,
            net,
            id,
        ));
    }

    let mut report = UpdateReport::default();
    if args.check || ctx.dry_run {
        for plan in plans {
            let result = match (&plan.skip, plan.needs, plan.no_source) {
                (Some(skip), _, _) => skip.clone(),
                (None, true, true) => RowResult::NewestUnknown,
                (None, true, false) => RowResult::Available,
                (None, false, _) => RowResult::Current,
            };
            report.rows.push(UpdateRow {
                tool: plan.id.to_string(),
                installed: plan.installed,
                newest: plan.newest,
                result,
                note: plan.note,
            });
        }
        return Ok(report);
    }

    if plans.iter().any(|plan| plan.skip.is_none() && plan.needs) {
        let rtk_needed = plans
            .iter()
            .any(|plan| plan.id == "rtk" && plan.skip.is_none() && plan.needs);
        back_up_first(ctx, hosts, lock.is_some(), rtk_needed)?;
    }
    for plan in plans {
        let row = match plan.skip.clone() {
            Some(result) => UpdateRow {
                tool: plan.id.to_string(),
                installed: plan.installed.clone(),
                newest: plan.newest.clone(),
                result,
                note: plan.note.clone(),
            },
            None if !plan.needs => UpdateRow {
                tool: plan.id.to_string(),
                installed: plan.installed.clone(),
                newest: plan.newest.clone(),
                result: RowResult::Current,
                note: None,
            },
            None => {
                let (row, restart) = match plan.id {
                    "node" => node_flow(ctx, search, hosts, &plan, &mut report),
                    "rtk" => rtk_flow(&engine, &plan, lock.as_ref(), &mut report),
                    _ => tool_flow(&engine, hosts, &plan, lock.as_ref(), &mut report),
                };
                for kind in restart {
                    if !report.restart.contains(&kind) {
                        report.restart.push(kind);
                    }
                }
                row
            }
        };
        report.rows.push(row);
    }
    report.restart.sort();
    Ok(report)
}

/// Decides one row without changing anything: its installed and newest versions, and whether a tool is skipped.
fn plan<'r>(
    engine: &Engine<'_>,
    recipes: &'r [Recipe],
    lock: Option<&Lockfile>,
    node: Option<&NodeInstall>,
    hosts: &[Box<dyn Host>],
    net: &dyn Net,
    id: &'static str,
) -> Plan<'r> {
    let mut plan = Plan {
        id,
        recipe: None,
        installed: None,
        newest: None,
        no_source: false,
        needs: false,
        skip: None,
        note: None,
    };
    let locked = lock.and_then(|lock| lock.tools.get(id));
    match id {
        "nvm" | "uv" => {
            plan.installed = locked.map(|entry| entry.version.clone());
            plan.newest = Some("pinned".to_string());
            plan.skip = Some(RowResult::NotTouched(
                "pinned (excluded from latest)".to_string(),
            ));
        }
        "gh" => {
            plan.installed = locked.map(|entry| entry.version.clone());
            plan.skip = Some(RowResult::NotTouched(if locked.is_some() {
                "gh is updated by its own route, which this build does not drive".to_string()
            } else {
                "not installed by relaxkit (no lock entry)".to_string()
            }));
        }
        "node" => match node {
            None => {
                plan.skip = Some(RowResult::NotTouched(
                    "no Node link; run `relaxkit setup` first".to_string(),
                ));
            }
            Some(now) => {
                let installed = now.version.trim_start_matches('v').to_string();
                plan.installed = Some(installed.clone());
                match latest_node_24(net) {
                    Err(err) => {
                        plan.skip = Some(RowResult::NewestUnknown);
                        plan.note = Some(format!("{err:#}"));
                    }
                    Ok(newest) => {
                        plan.needs = newest != installed;
                        plan.newest = Some(newest);
                        if !plan.needs {
                            plan.skip = Some(RowResult::Current);
                        }
                    }
                }
            }
        },
        _ => {
            let Some(recipe) = recipes.iter().find(|recipe| recipe.id == id) else {
                plan.skip = Some(RowResult::NotTouched("not in this build".to_string()));
                return plan;
            };
            plan.recipe = Some(recipe);
            let Some(entry) = locked else {
                plan.skip = Some(RowResult::NotTouched(
                    "not installed by relaxkit (no lock entry)".to_string(),
                ));
                return plan;
            };
            plan.installed = Some(entry.version.clone());
            let present = present_hosts(recipe, entry, hosts);
            if present.is_empty() {
                plan.skip = Some(RowResult::NotTouched(
                    "none of its hosts is installed now".to_string(),
                ));
                return plan;
            }
            if matches!(recipe.version, VersionSource::None) {
                plan.no_source = true;
                plan.needs = true;
                return plan;
            }
            match engine.latest_version(recipe) {
                Err(err) => {
                    plan.skip = Some(RowResult::NewestUnknown);
                    plan.note = Some(format!("{err:#}"));
                }
                Ok(newest) => {
                    plan.needs = present
                        .iter()
                        .any(|kind| !at_newest(*kind, entry.version_for(kind.as_str()), &newest));
                    plan.newest = Some(newest);
                    if !plan.needs {
                        plan.skip = Some(RowResult::Current);
                    }
                }
            }
        }
    }
    plan
}

/// Whether a locked version is the newest release. A Claude Code listing version carries a build suffix after its
/// first `-` (such as `7.0.2-22f0b463e7a5`) that the release tag does not carry, so for the Claude host only the part
/// before the first `-` is compared (F3-2). Other hosts compare the whole version.
fn at_newest(kind: HostKind, locked: &str, newest: &str) -> bool {
    match kind {
        HostKind::Claude => locked.split('-').next() == Some(newest),
        _ => locked == newest,
    }
}

/// The hosts a locked tool is on, that the recipe names, and that are installed now, in host order.
fn present_hosts(recipe: &Recipe, entry: &LockedTool, hosts: &[Box<dyn Host>]) -> Vec<HostKind> {
    recipe
        .hosts
        .iter()
        .copied()
        .filter(|kind| entry.hosts.iter().any(|name| name == kind.as_str()))
        .filter(|kind| hosts.iter().any(|host| host.info().kind == *kind))
        .collect()
}

/// The recipe limited to the hosts its lock names, so an update never adds a host the pack did not install there.
fn scoped(recipe: &Recipe, entry: &LockedTool) -> Recipe {
    let mut copy = recipe.clone();
    copy.hosts
        .retain(|kind| entry.hosts.iter().any(|name| name == kind.as_str()));
    copy
}

/// Whether a host's route can return to a given version. A Claude Code plugin is updated, never pinned, so its
/// locked version cannot be put back (the host has no pinned install). Every other route is pinned.
fn pinnable(recipe: &Recipe, kind: HostKind) -> bool {
    !(kind == HostKind::Claude
        && recipe
            .install
            .get(&kind)
            .into_iter()
            .flatten()
            .any(|step| matches!(step, crate::recipe::Step::Plugin { .. })))
}

/// A Claude Code plugin's version as the host reports it now, when the host reports one.
fn reported_version(
    hosts: &[Box<dyn Host>],
    ctx: &Ctx,
    kind: HostKind,
    name: &str,
) -> Option<String> {
    let host = hosts.iter().find(|host| host.info().kind == kind)?;
    host.plugin_list(ctx)
        .ok()?
        .into_iter()
        .find(|plugin| plugin.name == name)
        .and_then(|plugin| plugin.version)
}

/// Records each host's new version in the lock entry of `id`, one host at a time, so every other host keeps the
/// version it had (K73, K74). The route is kept unless one is given. Only exact versions are recorded.
fn record_moves(
    ctx: &Ctx,
    id: &str,
    moves: &[(HostKind, String)],
    route: Option<&str>,
) -> Result<()> {
    let mut lock = Lockfile::load(ctx)?.unwrap_or_else(|| Lockfile {
        relaxkit: env!("CARGO_PKG_VERSION").to_string(),
        tools: Default::default(),
    });
    let mut entry = lock
        .tools
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("`{id}` has no lock entry to move"))?;
    let route = route
        .map(str::to_string)
        .unwrap_or_else(|| entry.route.clone());
    for (kind, version) in moves {
        crate::state::lockfile::check_version(version)?;
        entry = entry.moved(&[kind.to_string()], version, &route);
    }
    lock.tools.insert(id.to_string(), entry);
    lock.save(ctx, &ctx.run)
}

/// The Node row (K66, K81): `nvm install 24` through the pinned nvm with `PROFILE=/dev/null`, then the new Node is
/// probed by its own path, and only then does the fixed link move. The previous Node stays installed.
fn node_flow(
    ctx: &Ctx,
    search: &NvmSearch,
    hosts: &[Box<dyn Host>],
    plan: &Plan<'_>,
    report: &mut UpdateReport,
) -> (UpdateRow, Vec<HostKind>) {
    let row = |result: RowResult, note: Option<String>| UpdateRow {
        tool: plan.id.to_string(),
        installed: plan.installed.clone(),
        newest: plan.newest.clone(),
        result,
        note,
    };
    let outcome = update_node_in(ctx, search).and_then(|next| {
        let node_bin = next.root.join("bin").join("node");
        let out = ctx
            .runner
            .run(&Cmd::new(&node_bin.display().to_string()).arg("--version"))
            .with_context(|| format!("the new Node at {} did not run", node_bin.display()))?;
        if out.code != 0 || !out.stdout.contains(&next.version) {
            bail!(
                "the new Node {} did not report its version, so the link was not moved",
                next.version
            );
        }
        record_link(ctx, &ctx.run, &ctx.dirs.data.join("node"))?;
        link_node(ctx, &next)?;
        Ok(next)
    });
    match outcome {
        Ok(next) => {
            let all: Vec<HostKind> = hosts.iter().map(|host| host.info().kind).collect();
            (
                row(
                    RowResult::Updated,
                    Some(format!("link now {}", next.version)),
                ),
                all,
            )
        }
        Err(err) => {
            report.failed.push(plan.id.to_string());
            (
                row(RowResult::RolledBack, Some(format!("{err:#}"))),
                Vec::new(),
            )
        }
    }
}

/// The rtk row (K65): the latest release is written to the pack copy through the install path. A link at the pack path
/// is removed before the write; a brew copy is never upgraded by brew. A failure puts the pack copy back as it was.
fn rtk_flow(
    engine: &Engine<'_>,
    plan: &Plan<'_>,
    lock: Option<&Lockfile>,
    report: &mut UpdateReport,
) -> (UpdateRow, Vec<HostKind>) {
    let ctx = engine.ctx;
    let row = |result: RowResult, note: Option<String>| UpdateRow {
        tool: plan.id.to_string(),
        installed: plan.installed.clone(),
        newest: plan.newest.clone(),
        result,
        note,
    };
    let (Some(recipe), Some(entry)) = (plan.recipe, lock.and_then(|l| l.tools.get(plan.id))) else {
        return (
            row(
                RowResult::NotTouched("not installed by relaxkit".to_string()),
                None,
            ),
            Vec::new(),
        );
    };
    let target = plan.newest.clone().unwrap_or_default();
    let bin = ctx.dirs.bin.join("rtk");
    let note = (entry.route == "brew").then(|| {
        format!(
            "a brew-owned rtk is not upgraded by brew: the pack copy at {} is the one relaxkit manages, and it was updated",
            bin.display()
        )
    });
    // Owner ruling K87: while any installed host is below rtk's minimum, the shared pack binary is not moved and no lock
    // entry is written. Moving it would change what the other host runs while the lock keeps the old version. Relaxkit
    // does not update the host; the owner updates it through the host's own updater.
    let below: Vec<String> = recipe
        .hosts
        .iter()
        .filter_map(|kind| {
            let host = engine.hosts.iter().find(|host| host.info().kind == *kind)?;
            let need = recipe.minimum_host.get(kind)?;
            match check_minimum(host.info(), need) {
                HostCheck::TooOld { have, need } => Some(format!(
                    "{} {have} is below rtk's minimum {need}; update {}, then run `relaxkit update rtk`",
                    kind.product_name(),
                    kind.product_name()
                )),
                HostCheck::Ok => None,
            }
        })
        .collect();
    if let Some(reason) = below.into_iter().next() {
        return (row(RowResult::NotTouched(reason), note), Vec::new());
    }
    let before = match FileState::capture(&bin) {
        Ok(before) => before,
        Err(err) => {
            report.failed.push(plan.id.to_string());
            return (
                row(RowResult::NotTouched(format!("{err:#}")), note),
                Vec::new(),
            );
        }
    };
    match engine.update(&scoped(recipe, entry), &target) {
        // No host was new enough, so nothing moved: the lock keeps its route and version.
        Ok(out) if out.updated.is_empty() => (
            row(
                RowResult::NotTouched("no installed host is new enough".to_string()),
                None,
            ),
            Vec::new(),
        ),
        Ok(out) => {
            // Only a host whose lock version differs moved (K72). A host already at the target is not a move, so when
            // none remain the row is current, the lock is not written, and no host restarts.
            let moved: Vec<HostKind> = out
                .updated
                .iter()
                .copied()
                .filter(|kind| entry.version_for(kind.as_str()) != target)
                .collect();
            if moved.is_empty() {
                return (row(RowResult::Current, note), Vec::new());
            }
            match record_moves(ctx, plan.id, &moves_to(&moved, &target), Some("script")) {
                Ok(()) => (row(RowResult::Updated, note), moved),
                Err(err) => {
                    report.failed.push(plan.id.to_string());
                    // The pack copy was written before the lock failed, so the hosts are still listed to restart (K69).
                    (
                        row(RowResult::RollbackFailed(format!("{err:#}")), note),
                        out.updated,
                    )
                }
            }
        }
        Err(err) => {
            report.failed.push(plan.id.to_string());
            // The hosts' configuration may have been rewritten before the failure, so they are listed to restart (K69).
            let changed = present_hosts(recipe, entry, engine.hosts);
            match before.restore(&bin) {
                Ok(()) => (
                    row(RowResult::RolledBack, Some(format!("{err:#}"))),
                    changed,
                ),
                Err(restore) => (
                    row(
                        RowResult::RollbackFailed(format!("{restore:#}")),
                        Some(format!("{err:#}")),
                    ),
                    changed,
                ),
            }
        }
    }
}

/// Any other tool: the route moves it on each host it is locked on, the probe confirms it, and a failure returns it
/// to its locked version (K67). A Claude Code plugin has no pin to return to, so its failure is named, not undone.
fn tool_flow(
    engine: &Engine<'_>,
    hosts: &[Box<dyn Host>],
    plan: &Plan<'_>,
    lock: Option<&Lockfile>,
    report: &mut UpdateReport,
) -> (UpdateRow, Vec<HostKind>) {
    let ctx = engine.ctx;
    let row = |result: RowResult, note: Option<String>| UpdateRow {
        tool: plan.id.to_string(),
        installed: plan.installed.clone(),
        newest: plan.newest.clone(),
        result,
        note,
    };
    let (Some(recipe), Some(entry)) = (plan.recipe, lock.and_then(|l| l.tools.get(plan.id))) else {
        return (
            row(
                RowResult::NotTouched("not installed by relaxkit".to_string()),
                None,
            ),
            Vec::new(),
        );
    };
    let scope = scoped(recipe, entry);
    let present = present_hosts(recipe, entry, hosts);
    // The version each present host returns to, and whether a host can be returned at all.
    let old_versions: BTreeSet<String> = present
        .iter()
        .map(|kind| entry.version_for(kind.as_str()).to_string())
        .collect();
    let every_pinnable = present.iter().all(|kind| pinnable(recipe, *kind));
    let target = plan.newest.clone().unwrap_or_default();

    // The sandbox plugin moves only after the self-test passes (K68). A failure keeps the locked version and names it.
    if plan.id == "opencode-sandbox" {
        let host = hosts
            .iter()
            .find(|host| host.info().kind == HostKind::Opencode);
        if let Some(host) = host {
            let kept = plan.installed.clone().unwrap_or_default();
            // The candidate is proven with the runtime it declares. Without that metadata nothing can be proven, so
            // nothing moves and the tool is named.
            let runtime = match candidate_runtime(ctx, &target) {
                Ok(runtime) => runtime,
                Err(err) => {
                    report.failed.push(plan.id.to_string());
                    return (
                        row(
                            RowResult::NotTouched(format!(
                                "{target} declares no readable runtime, so the locked {kept} stays"
                            )),
                            Some(format!("{err:#}")),
                        ),
                        Vec::new(),
                    );
                }
            };
            let failing: Vec<&str> = selftest_with(ctx, host.as_ref(), &runtime)
                .iter()
                .filter(|probe| !probe.passed)
                .map(|probe| probe.probe)
                .collect();
            if !failing.is_empty() {
                report.failed.push(plan.id.to_string());
                return (
                    row(
                        RowResult::NotTouched(format!(
                            "the sandbox self-test on runtime {runtime} failed for {target}, so the locked {kept} stays"
                        )),
                        Some(failing.join("; ")),
                    ),
                    Vec::new(),
                );
            }
        }
    }
    match engine.update(&scope, &target) {
        Ok(out) => {
            if out.updated.is_empty() {
                return (
                    row(
                        RowResult::NotTouched("no installed host is new enough".to_string()),
                        None,
                    ),
                    Vec::new(),
                );
            }
            // A pinned route records the version it asked for. A Claude Code plugin records the version its host
            // reports, because the marketplace decides it. A host that reports no exact version is a failure: the
            // change cannot be recorded, so it is named rather than left unrecorded.
            let mut moves = Vec::new();
            for kind in &out.updated {
                let version = if pinnable(recipe, *kind) {
                    target.clone()
                } else {
                    match reported_version(hosts, ctx, *kind, plan.id)
                        .filter(|version| crate::state::lockfile::check_version(version).is_ok())
                    {
                        Some(version) => version,
                        None => {
                            report.failed.push(plan.id.to_string());
                            return (
                                row(
                                    RowResult::RollbackFailed(format!(
                                        "{kind} reports no exact version after the update, so the new version cannot be recorded"
                                    )),
                                    None,
                                ),
                                out.updated.clone(),
                            );
                        }
                    }
                };
                moves.push((*kind, version));
            }
            let changed: Vec<(HostKind, String)> = moves
                .into_iter()
                .filter(|(kind, version)| entry.version_for(kind.as_str()) != version)
                .collect();
            if changed.is_empty() {
                return (
                    row(
                        RowResult::Current,
                        Some("the host already runs the version it serves".to_string()),
                    ),
                    Vec::new(),
                );
            }
            let restart: Vec<HostKind> = changed.iter().map(|(kind, _)| *kind).collect();
            match record_moves(ctx, plan.id, &changed, None) {
                Ok(()) => {
                    // A move from a version that was never recorded records the version now. It is not an update of a
                    // version the lock knew, so it carries no "updated" label; the hosts still restart (F3-1).
                    let from_unversioned = changed
                        .iter()
                        .any(|(kind, _)| entry.version_for(kind.as_str()) == UNVERSIONED);
                    let result = if from_unversioned {
                        row(
                            RowResult::Current,
                            Some("the previous version was not recorded, so the new version is recorded now".to_string()),
                        )
                    } else {
                        row(RowResult::Updated, None)
                    };
                    (result, restart.clone())
                }
                Err(err) => {
                    report.failed.push(plan.id.to_string());
                    (
                        row(RowResult::RollbackFailed(format!("{err:#}")), None),
                        restart,
                    )
                }
            }
        }
        Err(err) => {
            report.failed.push(plan.id.to_string());
            let note = Some(format!("{err:#}"));
            // The hosts were updated before the probe failed, so they are listed to restart whatever the result (K69).
            let changed = present.clone();
            if !every_pinnable || old_versions.len() != 1 {
                let reason = if !every_pinnable {
                    "a Claude Code plugin cannot be pinned, so its locked version cannot be restored".to_string()
                } else {
                    "its hosts are on different versions, so one locked version cannot be restored"
                        .to_string()
                };
                return (row(RowResult::RollbackFailed(reason), note), changed);
            }
            let old = old_versions.into_iter().next().unwrap_or_default();
            match engine.update(&scope, &old) {
                Ok(_) => (row(RowResult::RolledBack, note), changed),
                Err(again) => (
                    row(RowResult::RollbackFailed(format!("{again:#}")), note),
                    changed,
                ),
            }
        }
    }
}

/// The per-host moves of a pinned route: every host that moved goes to `version`.
fn moves_to(hosts: &[HostKind], version: &str) -> Vec<(HostKind, String)> {
    hosts
        .iter()
        .map(|kind| (*kind, version.to_string()))
        .collect()
}

/// The state of one file before a change, so a rolled-back rtk gets back exactly what it had.
enum FileState {
    Absent,
    Bytes { text: Vec<u8>, mode: u32 },
    Link(PathBuf),
}

impl FileState {
    fn capture(path: &Path) -> Result<FileState> {
        match fs::symlink_metadata(path) {
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(FileState::Absent),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
            Ok(meta) if meta.file_type().is_symlink() => {
                Ok(FileState::Link(fs::read_link(path).with_context(|| {
                    format!("cannot read the link {}", path.display())
                })?))
            }
            Ok(meta) => Ok(FileState::Bytes {
                text: fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
                mode: meta.permissions().mode(),
            }),
        }
    }

    /// Puts the file back as it was. A link is put back as a link to the same target.
    fn restore(&self, path: &Path) -> Result<()> {
        if path.symlink_metadata().is_ok() {
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
        match self {
            FileState::Absent => Ok(()),
            FileState::Bytes { text, mode } => {
                write_atomic(path, text)?;
                fs::set_permissions(path, fs::Permissions::from_mode(*mode))
                    .with_context(|| format!("cannot restore the mode of {}", path.display()))
            }
            FileState::Link(target) => std::os::unix::fs::symlink(target, path)
                .with_context(|| format!("cannot restore the link {}", path.display())),
        }
    }
}

/// Backs up everything the run may touch, before any change (K67): the lock snapshot and its journal record, the
/// existing user files of each detected host, the sandbox policy, and the pack's rtk copy. The rtk copy is journaled
/// even when absent, because the release is written as a new file, and undo must remove it again. A link is not backed
/// up: a backup would read the file behind it, and undo does not restore links.
fn back_up_first(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    has_lock: bool,
    rtk_needed: bool,
) -> Result<()> {
    if has_lock && let Some(lock) = Lockfile::load(ctx)? {
        lock.snapshot(ctx, &ctx.run)?;
    }
    let rtk = ctx.dirs.bin.join("rtk");
    let mut candidates: Vec<PathBuf> = vec![
        ctx.dirs.config.join("lock.toml"),
        sandbox_config_path(ctx),
        rtk.clone(),
    ];
    for host in hosts {
        candidates.extend(host.state_files(ctx));
        candidates.push(host.config_file(ctx));
    }
    let mut seen = BTreeSet::new();
    for file in candidates {
        if !seen.insert(file.clone()) {
            continue;
        }
        if file
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            continue;
        }
        // An absent file is journaled only where this run writes it without `safe_write`: the pack rtk copy. The
        // sandbox policy and host configs are journaled by `safe_write` just before a write, so a file the update never
        // writes is never recorded as created (undo would otherwise delete a file the user made later).
        let may_create = file == rtk && rtk_needed;
        if !file.exists() && !may_create {
            continue;
        }
        record_backup(ctx, &ctx.run, &file)?;
    }
    Ok(())
}

/// The text of the report, one line per row, then any failures, then the restart list, which is always last (K69).
pub fn lines(report: &UpdateReport) -> Vec<String> {
    let mut lines = Vec::new();
    for row in &report.rows {
        let installed = row.installed.as_deref().unwrap_or("-");
        let newest = row.newest.as_deref().unwrap_or("unknown");
        let note = row
            .note
            .as_deref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        lines.push(format!(
            "{:<22} {installed} -> {newest}: {}{note}",
            row.tool,
            row.result.label()
        ));
    }
    if !report.failed.is_empty() {
        lines.push(format!("failed: {}", report.failed.join(", ")));
    }
    if !report.restart.is_empty() {
        let names: Vec<&str> = report
            .restart
            .iter()
            .map(|kind| kind.product_name())
            .collect();
        lines.push(format!(
            "restart: {} (hooks and plugins load when a session starts)",
            names.join(" and ")
        ));
    }
    lines
}

/// The report as JSON (K70 and K75): rows of {tool, installed, newest, result, note}, then restart and failed.
pub fn to_json(report: &UpdateReport) -> Value {
    json!({
        "rows": report.rows.iter().map(|row| json!({
            "tool": row.tool,
            "installed": row.installed,
            "newest": row.newest,
            "result": row.result.label(),
            "note": row.note,
        })).collect::<Vec<_>>(),
        "restart": report.restart.iter().map(|kind| kind.as_str()).collect::<Vec<_>>(),
        "failed": report.failed,
    })
}
