//! `relaxkit status` (spec section 6): versions, drift, enforced-vs-advisory settings, the sandbox plugin's health
//! and `rtk gain`. Read-only: it writes nothing and runs only detection and read commands.
//!
//! The report is typed: each table row is a value, and `render` lays the rows out as text. Tests read the values.

use anyhow::Result;

use crate::adapters::OPENCODE_PLUGIN;
use crate::commands::read_optional;
use crate::commands::setup::opencode_models;
use crate::config::{KitConfig, effective};
use crate::config_apply::claude::{ROLES_BLOCK, roles_block_text};
use crate::config_apply::opencode::{ROUTING_BLOCK, ROUTING_TEXT};
use crate::ctx::Ctx;
use crate::edit::{block_get, jsonc_get};
use crate::exec::Cmd;
use crate::host::{Host, HostCheck, HostKind, check_minimum, detect_hosts};
use crate::recipe::load_all;
use crate::recipe::schema::{Kind, Recipe};
use crate::roles::{Role, resolve_claude, resolve_opencode};
use crate::runtime::nvm::{NodeInstall, other_nodes};
use crate::sandbox::health::{health_line, plugin_health};
use crate::state::lockfile::Lockfile;
use crate::tools::Engine;
use crate::ui::{Cell, Mark, Palette, detail, heading, outcome, table};

/// How a locked tool stands on one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Installed at the locked version.
    AsLocked,
    /// Installed. The lock never recorded a version, so there is nothing to compare.
    AsLockedUnrecorded,
    /// Installed at another version than the lock.
    Drift,
    /// Not found on the host.
    NotFound,
    /// Found, but its version could not be read (the reason is a note).
    Unreadable,
    /// An MCP entry: status does not read it.
    McpNotRead,
    /// An npm tool with no install step: npx runs it at its pin, so there is no installed copy to find.
    RunsOnDemand,
    /// The host is not installed, so the tool is not read on it.
    HostMissing,
}

impl ToolState {
    /// The mark and the words a table shows for this state.
    fn mark_and_text(self) -> (Mark, &'static str) {
        match self {
            ToolState::AsLocked => (Mark::Ok, "as locked"),
            ToolState::AsLockedUnrecorded => (Mark::Ok, "as locked, version not recorded"),
            ToolState::Drift => (Mark::Warn, "drift"),
            ToolState::NotFound => (Mark::Fail, "not found"),
            ToolState::Unreadable => (Mark::Fail, "unreadable"),
            ToolState::McpNotRead => (Mark::Skip, "MCP entry not read"),
            ToolState::RunsOnDemand => (Mark::Skip, "runs through npx"),
            ToolState::HostMissing => (Mark::Skip, "host not installed"),
        }
    }
}

/// Whether a tool runs on demand on this host: an npm tool with no install step there. Its install step is what puts
/// a copy on the host, so a tool with one (a plugin, an MCP entry, a command) is not on demand.
fn runs_on_demand(recipe: &Recipe, kind: HostKind) -> bool {
    recipe.kind == Kind::Npm && recipe.install.get(&kind).is_none_or(Vec::is_empty)
}

/// One locked tool on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub id: String,
    pub host: HostKind,
    /// The version found on the host, or `None` when it is not found, not read, or not checked.
    pub installed: Option<String>,
    pub locked: String,
    pub route: String,
    pub state: ToolState,
}

/// One role across both hosts: its setting, and what each host runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRow {
    pub role: &'static str,
    pub setting: String,
    pub claude: String,
    pub opencode: String,
}

/// Whether one host's sandbox is enforcing, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRow {
    pub host: HostKind,
    pub enforced: bool,
    pub detail: String,
}

/// A line of notes: drift, a host that is too old, a missing copy, or a fact such as another Node on the PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub mark: Mark,
    pub text: String,
}

impl Note {
    fn warn(text: impl Into<String>) -> Note {
        Note {
            mark: Mark::Warn,
            text: text.into(),
        }
    }

    fn info(text: impl Into<String>) -> Note {
        Note {
            mark: Mark::Skip,
            text: text.into(),
        }
    }
}

/// What `status` shows, as values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusReport {
    pub version: String,
    pub preset: String,
    /// Each host found, with its version.
    pub hosts: Vec<(HostKind, String)>,
    pub tools: Vec<ToolRow>,
    pub roles: Vec<RoleRow>,
    pub sandbox: Vec<SandboxRow>,
    pub notes: Vec<Note>,
    /// The savings line of `rtk gain`, or `n/a`.
    pub rtk_gain: String,
}

impl StatusReport {
    /// The report as plain text, with no color. Tests and pipes read this.
    pub fn lines(&self) -> Vec<String> {
        self.render(Palette::PLAIN)
    }

    /// The count of rows that need the owner: a tool that is missing, unreadable or drifted, and a sandbox that does
    /// not enforce. Returns the mark of the worst of them and the words that say what they are.
    fn verdict(&self) -> (Mark, String) {
        // Nothing was checked when no host is found or nothing is locked, so the verdict must not say all is well.
        if self.hosts.is_empty() {
            return (Mark::Warn, "no host found on the PATH".to_string());
        }
        if self.tools.is_empty() {
            return (
                Mark::Skip,
                "nothing is locked yet; run relaxkit setup".to_string(),
            );
        }
        let count =
            |wanted: fn(&ToolRow) -> bool| self.tools.iter().filter(|row| wanted(row)).count();
        let not_found = count(|row| row.state == ToolState::NotFound);
        let unreadable = count(|row| row.state == ToolState::Unreadable);
        let drift = count(|row| row.state == ToolState::Drift);
        let not_enforced = self.sandbox.iter().filter(|row| !row.enforced).count();
        let mut parts = Vec::new();
        if not_found > 0 {
            parts.push(format!("{not_found} not found"));
        }
        if unreadable > 0 {
            parts.push(format!("{unreadable} unreadable"));
        }
        if drift > 0 {
            parts.push(format!("{drift} drift"));
        }
        if not_enforced > 0 {
            parts.push(format!("{not_enforced} sandbox not enforced"));
        }
        if parts.is_empty() {
            return (Mark::Ok, "everything is as locked".to_string());
        }
        let mark = if not_found + unreadable + not_enforced > 0 {
            Mark::Fail
        } else {
            Mark::Warn
        };
        (mark, format!("need attention: {}", parts.join(", ")))
    }

    /// The text `status` prints. `palette` decides color and glyphs, so a terminal gets them and a pipe does not.
    pub fn render(&self, palette: Palette) -> Vec<String> {
        let (verdict_mark, verdict) = self.verdict();
        let mut out = vec![
            heading(
                &format!("relaxkit {} · preset {}", self.version, self.preset),
                palette,
            ),
            outcome(palette, verdict_mark, &verdict),
        ];
        if !self.hosts.is_empty() {
            out.push(String::new());
            out.push(heading("Hosts", palette));
            let rows: Vec<Vec<Cell>> = self
                .hosts
                .iter()
                .map(|(kind, version)| vec![Cell::new(kind.product_name()), Cell::new(version)])
                .collect();
            out.extend(table(&["HOST", "VERSION"], &rows, 2, palette));
        }
        if !self.tools.is_empty() {
            out.push(String::new());
            out.push(heading("Tools", palette));
            let rows: Vec<Vec<Cell>> = self
                .tools
                .iter()
                .map(|row| {
                    let (mark, text) = row.state.mark_and_text();
                    vec![
                        Cell::new(&row.id),
                        Cell::new(row.host.to_string()),
                        Cell::new(row.installed.as_deref().unwrap_or("-")),
                        Cell::new(&row.locked),
                        Cell::marked(mark, text),
                        Cell::new(format!("({})", row.route)),
                    ]
                })
                .collect();
            out.extend(table(
                &["TOOL", "HOST", "INSTALLED", "LOCKED", "STATE", "ROUTE"],
                &rows,
                2,
                palette,
            ));
        }
        if !self.roles.is_empty() {
            out.push(String::new());
            out.push(heading("Roles", palette));
            let rows: Vec<Vec<Cell>> = self
                .roles
                .iter()
                .map(|row| {
                    vec![
                        Cell::new(row.role),
                        Cell::new(&row.setting),
                        Cell::new(&row.claude),
                        Cell::new(&row.opencode),
                    ]
                })
                .collect();
            out.extend(table(
                &["ROLE", "SETTING", "CLAUDE CODE", "OPENCODE"],
                &rows,
                2,
                palette,
            ));
        }
        if !self.sandbox.is_empty() {
            out.push(String::new());
            out.push(heading("Sandbox", palette));
            let rows: Vec<Vec<Cell>> = self
                .sandbox
                .iter()
                .map(|row| {
                    let (mark, text) = if row.enforced {
                        (Mark::Ok, "enforced")
                    } else {
                        (Mark::Fail, "not enforced")
                    };
                    vec![
                        Cell::new(row.host.product_name()),
                        Cell::marked(mark, text),
                        Cell::new(&row.detail),
                    ]
                })
                .collect();
            out.extend(table(&["HOST", "SANDBOX", "DETAIL"], &rows, 2, palette));
        }
        if !self.notes.is_empty() {
            out.push(String::new());
            out.push(heading("Notes", palette));
            for note in &self.notes {
                out.push(detail(palette, note.mark, &note.text));
            }
        }
        out.push(String::new());
        out.push(format!("rtk gain: {}", self.rtk_gain));
        out
    }
}

/// `status` on the hosts found on the PATH.
pub fn run(ctx: &Ctx) -> Result<StatusReport> {
    let hosts = detect_hosts(ctx);
    report_for(ctx, &hosts)
}

/// `status` for the given hosts. Tests use this so they do not depend on the PATH or on environment variables.
pub fn report_for(ctx: &Ctx, hosts: &[Box<dyn Host>]) -> Result<StatusReport> {
    let config = KitConfig::load(&ctx.dirs)?;
    let lock = Lockfile::load(ctx)?;
    let recipes = load_all()?;
    let mut notes = Vec::new();
    if hosts.is_empty() {
        notes.push(Note::info("no host found on the PATH"));
    }

    // Installed against locked, per host, and whether each host is too old for the tool.
    let node = NodeInstall {
        version: String::new(),
        root: ctx.dirs.data.join("node"),
    };
    let engine = Engine {
        ctx,
        run: &ctx.run,
        hosts,
        recipes: &recipes,
        node: &node,
    };
    let mut tools = Vec::new();
    if let Some(lock) = &lock {
        for (id, tool) in &lock.tools {
            // gh is not a recipe, and status does not read its version (parked). A missing pack copy is the one note
            // status shows for it (F7-2, K84).
            if id == crate::runtime::gh::GH_ID {
                let bin = ctx.dirs.bin.join("gh");
                if !crate::platform::is_executable(&bin) {
                    notes.push(Note::warn(format!(
                        "gh: pack copy missing at {} (locked {})",
                        bin.display(),
                        tool.version
                    )));
                }
                continue;
            }
            let Some(recipe) = recipes.iter().find(|recipe| &recipe.id == id) else {
                notes.push(Note::warn(format!(
                    "{id}: locked {} but no recipe carries it",
                    tool.version
                )));
                continue;
            };
            for host_name in &tool.hosts {
                let Ok(kind) = host_name.parse::<HostKind>() else {
                    continue;
                };
                // Each host is compared with the version its own lock entry records (K73).
                let locked = tool.version_for(host_name).to_string();
                let route = tool.route.clone();
                let Some(host) = hosts.iter().find(|host| host.info().kind == kind) else {
                    tools.push(ToolRow {
                        id: id.clone(),
                        host: kind,
                        installed: None,
                        locked,
                        route,
                        state: ToolState::HostMissing,
                    });
                    continue;
                };
                // An MCP entry is not read by status (engine.detect returns nothing for it), so it is neither "not found"
                // nor compared (F5-2).
                if recipe.kind == Kind::Mcp {
                    tools.push(ToolRow {
                        id: id.clone(),
                        host: kind,
                        installed: None,
                        locked,
                        route,
                        state: ToolState::McpNotRead,
                    });
                } else if runs_on_demand(recipe, kind) {
                    tools.push(ToolRow {
                        id: id.clone(),
                        host: kind,
                        installed: None,
                        locked,
                        route,
                        state: ToolState::RunsOnDemand,
                    });
                } else {
                    let (installed, state) = match engine.detect(recipe, host.as_ref()) {
                        Ok(Some((version, _))) => {
                            // A version that was never recorded cannot drift: its host version is shown, not compared (F3-1).
                            let state = if locked == crate::tools::engine::UNVERSIONED {
                                ToolState::AsLockedUnrecorded
                            } else if version == locked {
                                ToolState::AsLocked
                            } else {
                                ToolState::Drift
                            };
                            (Some(version), state)
                        }
                        Ok(None) => (None, ToolState::NotFound),
                        Err(err) => {
                            notes.push(Note::warn(format!(
                                "{id} on {kind}: its version cannot be read: {err:#}"
                            )));
                            (None, ToolState::Unreadable)
                        }
                    };
                    tools.push(ToolRow {
                        id: id.clone(),
                        host: kind,
                        installed,
                        locked,
                        route,
                        state,
                    });
                }
                if let Some(need) = recipe.minimum_host.get(&kind)
                    && let HostCheck::TooOld { have, need } = check_minimum(host.info(), need)
                {
                    notes.push(Note::warn(format!(
                        "{id} on {kind}: host {have} is older than {need}, the oldest this tool supports"
                    )));
                }
            }
        }
    }

    let roles = role_rows(ctx, hosts, &config);
    drift_notes(ctx, hosts, &config, &mut notes)?;
    let sandbox = sandbox_rows(ctx, hosts)?;

    let others = other_nodes(ctx);
    if others.is_empty() {
        notes.push(Note::info("no other Node on the PATH"));
    }
    for (name, path) in others {
        notes.push(Note::info(format!(
            "other Node on the PATH: {name} at {} (not used)",
            path.display()
        )));
    }

    Ok(StatusReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        preset: config.preset.as_str().to_string(),
        hosts: hosts
            .iter()
            .map(|host| {
                let info = host.info();
                (info.kind, info.version.clone())
            })
            .collect(),
        tools,
        roles,
        sandbox,
        notes,
        rtk_gain: rtk_gain(ctx),
    })
}

/// Each role's setting, and what each host does with it: advisory, or enforced per agent on OpenCode.
fn role_rows(ctx: &Ctx, hosts: &[Box<dyn Host>], config: &KitConfig) -> Vec<RoleRow> {
    let available = hosts
        .iter()
        .find(|host| host.info().kind == HostKind::Opencode)
        .map(|host| opencode_models(ctx, host.as_ref()))
        .unwrap_or_default();
    Role::ALL
        .into_iter()
        .map(|role| {
            let setting = effective(config, role);
            // Each host shows the setting it runs with, which a `--host` override may change (K-6.1 rule 3).
            let host_cell = |kind: HostKind| -> String {
                if !hosts.iter().any(|host| host.info().kind == kind) {
                    return "-".to_string();
                }
                let own = effective(&config.for_host(kind), role);
                match kind {
                    HostKind::Claude => match resolve_claude(&own) {
                        Some(alias) => format!("{alias} · advisory"),
                        None => "host default".to_string(),
                    },
                    HostKind::Opencode => match resolve_opencode(&own, &available) {
                        Some(model) => {
                            let mode = if role == Role::Planner {
                                "advisory"
                            } else {
                                "enforced per agent"
                            };
                            format!("{model} · {mode}")
                        }
                        None => "host default".to_string(),
                    },
                }
            };
            RoleRow {
                role: role.key(),
                setting: setting.to_config_string(),
                claude: host_cell(HostKind::Claude),
                opencode: host_cell(HostKind::Opencode),
            }
        })
        .collect()
}

/// Managed blocks that no longer match what relaxkit writes (spec 6.1: `status` reports drift from hand edits).
fn drift_notes(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    config: &KitConfig,
    notes: &mut Vec<Note>,
) -> Result<()> {
    for host in hosts {
        let path = host.instructions_file(ctx);
        let text = read_optional(&path)?.unwrap_or_default();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match host.info().kind {
            HostKind::Claude => {
                let expected = roles_block_text(&config.for_host(HostKind::Claude));
                let actual = block_get(&text, ROLES_BLOCK);
                match (expected, actual) {
                    (Some(expected), Some(actual)) if actual.trim() == expected.trim() => {}
                    (None, None) => {}
                    (Some(_), None) => notes.push(Note::warn(format!(
                        "{name} roles block is missing; run relaxkit setup to write it"
                    ))),
                    _ => notes.push(Note::warn(format!(
                        "drift: the roles block in {name} differs from relaxkit's; run relaxkit setup to restore it"
                    ))),
                }
            }
            HostKind::Opencode => match block_get(&text, ROUTING_BLOCK) {
                Some(actual) if actual.trim() == ROUTING_TEXT => {}
                Some(_) => notes.push(Note::warn(format!(
                    "drift: the routing block in {name} differs from relaxkit's; run relaxkit setup to restore it"
                ))),
                None => notes.push(Note::warn(format!(
                    "{name} routing block is missing; run relaxkit setup to write it"
                ))),
            },
        }
    }
    Ok(())
}

/// Whether each host's sandbox is enforcing (spec 7): Claude Code's settings, and whether the OpenCode sandbox plugin
/// is listed.
fn sandbox_rows(ctx: &Ctx, hosts: &[Box<dyn Host>]) -> Result<Vec<SandboxRow>> {
    let mut rows = Vec::new();
    for host in hosts {
        let kind = host.info().kind;
        rows.push(match kind {
            HostKind::Claude => {
                let text = read_optional(&host.config_file(ctx))?.unwrap_or_default();
                let flag = |key: &str| {
                    jsonc_get(&text, &["sandbox", key]).ok().flatten()
                        == Some(serde_json::Value::Bool(true))
                };
                let enforced = flag("enabled") && flag("failIfUnavailable");
                SandboxRow {
                    host: kind,
                    enforced,
                    detail: if enforced {
                        "sandbox.enabled and failIfUnavailable are set".to_string()
                    } else {
                        "settings.json lacks the sandbox keys".to_string()
                    },
                }
            }
            HostKind::Opencode => {
                let listed = host
                    .plugin_list(ctx)?
                    .iter()
                    .any(|plugin| plugin.name == OPENCODE_PLUGIN);
                SandboxRow {
                    host: kind,
                    enforced: listed,
                    detail: if listed {
                        format!(
                            "the {OPENCODE_PLUGIN} plugin is listed; {}",
                            health_line(&plugin_health(ctx))
                        )
                    } else {
                        format!("the {OPENCODE_PLUGIN} plugin is not listed; run relaxkit setup")
                    },
                }
            }
        });
    }
    Ok(rows)
}

/// The savings line of `rtk gain`: the line that names the tokens saved (`Tokens saved: …`), or `n/a` when rtk
/// reports none. The title line above it is not the savings.
fn rtk_gain(ctx: &Ctx) -> String {
    // The same rtk every caller resolves: the pack copy first (ruling R3-2).
    let program =
        crate::runtime::rtk::resolve(ctx).unwrap_or_else(|| std::path::PathBuf::from("rtk"));
    match ctx
        .runner
        .run(&Cmd::new(&program.to_string_lossy()).arg("gain"))
    {
        Ok(out) if out.code == 0 => out
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| line.to_ascii_lowercase().contains("saved:"))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|| "n/a".to_string()),
        _ => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_with_no_host_does_not_claim_everything_is_as_locked() {
        let report = StatusReport {
            version: "0.1.0".to_string(),
            preset: "balanced".to_string(),
            rtk_gain: "n/a".to_string(),
            ..StatusReport::default()
        };

        let lines = report.lines();

        assert_eq!(lines[1], "!  no host found on the PATH", "{lines:?}");
    }

    #[test]
    fn a_status_with_hosts_and_no_lock_says_nothing_is_locked_yet() {
        let report = StatusReport {
            version: "0.1.0".to_string(),
            preset: "balanced".to_string(),
            hosts: vec![(HostKind::Claude, "2.1.330".to_string())],
            rtk_gain: "n/a".to_string(),
            ..StatusReport::default()
        };

        assert_eq!(
            report.verdict().1,
            "nothing is locked yet; run relaxkit setup"
        );
    }

    #[test]
    fn the_verdict_counts_what_needs_the_owner() {
        let row = |state| ToolRow {
            id: "caveman".to_string(),
            host: HostKind::Claude,
            installed: None,
            locked: "4.2.0".to_string(),
            route: "claude-plugin:caveman".to_string(),
            state,
        };
        let report = StatusReport {
            hosts: vec![(HostKind::Claude, "2.1.330".to_string())],
            tools: vec![
                row(ToolState::NotFound),
                row(ToolState::Drift),
                row(ToolState::AsLocked),
            ],
            ..StatusReport::default()
        };

        let (mark, text) = report.verdict();

        assert_eq!(mark, Mark::Fail);
        assert_eq!(text, "need attention: 1 not found, 1 drift");
    }

    #[test]
    fn a_tool_that_runs_through_npx_is_not_counted_as_attention() {
        let row = |state| ToolRow {
            id: "ccusage".to_string(),
            host: HostKind::Claude,
            installed: None,
            locked: "20.0.51".to_string(),
            route: "npm".to_string(),
            state,
        };
        let report = |tools| StatusReport {
            hosts: vec![(HostKind::Claude, "2.1.330".to_string())],
            tools,
            ..StatusReport::default()
        };

        let with_on_demand = report(vec![row(ToolState::RunsOnDemand), row(ToolState::AsLocked)]);
        let as_locked_only = report(vec![row(ToolState::AsLocked)]);

        assert_eq!(with_on_demand.verdict(), as_locked_only.verdict());
    }

    #[test]
    fn runs_on_demand_is_true_for_ccusage_only() {
        let recipes = load_all().unwrap();
        for kind in [HostKind::Claude, HostKind::Opencode] {
            let on_demand: Vec<&str> = recipes
                .iter()
                .filter(|recipe| runs_on_demand(recipe, kind))
                .map(|recipe| recipe.id.as_str())
                .collect();
            assert_eq!(on_demand, ["ccusage"], "{kind:?}");
        }
    }
}
