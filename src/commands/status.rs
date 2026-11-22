//! `relaxkit status` (spec section 6): versions, drift, enforced-vs-advisory settings, the sandbox plugin's health
//! and `rtk gain`. Read-only: it writes nothing and runs only detection and read commands.

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
use crate::recipe::schema::Kind;
use crate::roles::{Role, resolve_claude, resolve_opencode};
use crate::runtime::nvm::{NodeInstall, other_nodes};
use crate::sandbox::health::{health_line, plugin_health};
use crate::state::lockfile::Lockfile;
use crate::tools::Engine;

/// The lines `status` prints, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusReport {
    pub lines: Vec<String>,
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
    let mut lines = vec![
        format!("relaxkit {}", env!("CARGO_PKG_VERSION")),
        format!("preset {}", config.preset.as_str()),
    ];

    if hosts.is_empty() {
        lines.push("no host found on the PATH".to_string());
    }
    for host in hosts {
        let info = host.info();
        lines.push(format!("{} {}", info.kind.product_name(), info.version));
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
    if let Some(lock) = &lock {
        for (id, tool) in &lock.tools {
            // gh is not a recipe, and status does not read its version (parked). A missing pack copy is the one line
            // status shows for it (F7-2, K84).
            if id == crate::runtime::gh::GH_ID {
                let bin = ctx.dirs.bin.join("gh");
                if !crate::platform::is_executable(&bin) {
                    lines.push(format!(
                        "gh: pack copy missing at {} (locked {})",
                        bin.display(),
                        tool.version
                    ));
                }
                continue;
            }
            let Some(recipe) = recipes.iter().find(|recipe| &recipe.id == id) else {
                lines.push(format!(
                    "{id}: locked {} but no recipe carries it",
                    tool.version
                ));
                continue;
            };
            for host_name in &tool.hosts {
                let Ok(kind) = host_name.parse::<HostKind>() else {
                    continue;
                };
                let Some(host) = hosts.iter().find(|host| host.info().kind == kind) else {
                    lines.push(format!(
                        "{id} on {kind}: locked {}, host not installed",
                        tool.version_for(host_name)
                    ));
                    continue;
                };
                // Each host is compared with the version its own lock entry records (K73).
                let locked = tool.version_for(host_name);
                // An MCP entry is not read by status (engine.detect returns nothing for it), so it is neither "not found"
                // nor compared: its locked version is shown with that state (F5-2).
                if recipe.kind == Kind::Mcp {
                    lines.push(format!(
                        "{id} on {kind}: locked {locked} ({}); MCP entry not read",
                        tool.route
                    ));
                } else {
                    let installed = match engine.detect(recipe, host.as_ref()) {
                        Ok(Some((version, _))) => version,
                        Ok(None) => "not found".to_string(),
                        Err(err) => format!("unreadable ({err:#})"),
                    };
                    // A version that was never recorded cannot drift: its host version is shown, not compared (F3-1).
                    // A tool that is missing or unreadable says so first: neither can be compared with anything (F4-3).
                    let state = if installed == "not found" {
                        "not found".to_string()
                    } else if installed.starts_with("unreadable") {
                        "unreadable".to_string()
                    } else if locked == crate::tools::engine::UNVERSIONED {
                        "as locked (version not recorded)".to_string()
                    } else if installed == locked {
                        "as locked".to_string()
                    } else {
                        format!("drift: locked {locked}")
                    };
                    lines.push(format!(
                        "{id} on {kind}: installed {installed}, locked {locked} ({}); {state}",
                        tool.route
                    ));
                }
                if let Some(need) = recipe.minimum_host.get(&kind)
                    && let HostCheck::TooOld { have, need } = check_minimum(host.info(), need)
                {
                    lines.push(format!(
                        "{id} on {kind}: host {have} is older than {need}, the oldest this tool supports"
                    ));
                }
            }
        }
    }

    role_lines(ctx, hosts, &config, &mut lines);
    drift_lines(ctx, hosts, &config, &mut lines)?;
    sandbox_lines(ctx, hosts, &mut lines)?;

    let others = other_nodes(ctx);
    if others.is_empty() {
        lines.push("no other Node on the PATH".to_string());
    }
    for (name, path) in others {
        lines.push(format!(
            "other Node on the PATH: {name} at {} (not used)",
            path.display()
        ));
    }

    lines.push(format!("rtk gain: {}", rtk_gain(ctx)));
    Ok(StatusReport { lines })
}

/// Each role's setting, and what each host does with it: advisory, or enforced per agent on OpenCode.
fn role_lines(ctx: &Ctx, hosts: &[Box<dyn Host>], config: &KitConfig, lines: &mut Vec<String>) {
    let available = hosts
        .iter()
        .find(|host| host.info().kind == HostKind::Opencode)
        .map(|host| opencode_models(ctx, host.as_ref()))
        .unwrap_or_default();
    for role in Role::ALL {
        let setting = effective(config, role);
        let mut parts = Vec::new();
        for host in hosts {
            // Each host shows the setting it runs with, which a `--host` override may change (K-6.1 rule 3).
            let kind = host.info().kind;
            let own = effective(&config.for_host(kind), role);
            match kind {
                HostKind::Claude => parts.push(match resolve_claude(&own) {
                    Some(alias) => format!("claude {alias} (advisory)"),
                    None => "claude inherits the host default".to_string(),
                }),
                HostKind::Opencode => parts.push(match resolve_opencode(&own, &available) {
                    Some(model) => {
                        let mode = if role == Role::Planner {
                            "advisory"
                        } else {
                            "enforced per agent"
                        };
                        format!("opencode {model} ({mode})")
                    }
                    None => "opencode inherits the host default".to_string(),
                }),
            }
        }
        lines.push(format!(
            "{} {}: {}",
            role.key(),
            setting.to_config_string(),
            parts.join("; ")
        ));
    }
}

/// Managed blocks that no longer match what relaxkit writes (spec 6.1: `status` reports drift from hand edits).
fn drift_lines(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    config: &KitConfig,
    lines: &mut Vec<String>,
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
                    (Some(_), None) => lines.push(format!(
                        "{name} roles block is missing; run relaxkit setup to write it"
                    )),
                    _ => lines.push(format!(
                        "drift: the roles block in {name} differs from relaxkit's; run relaxkit setup to restore it"
                    )),
                }
            }
            HostKind::Opencode => match block_get(&text, ROUTING_BLOCK) {
                Some(actual) if actual.trim() == ROUTING_TEXT => {}
                Some(_) => lines.push(format!(
                    "drift: the routing block in {name} differs from relaxkit's; run relaxkit setup to restore it"
                )),
                None => lines.push(format!(
                    "{name} routing block is missing; run relaxkit setup to write it"
                )),
            },
        }
    }
    Ok(())
}

/// Whether each host's sandbox is enforcing (spec 7): Claude Code's settings, and whether the OpenCode sandbox plugin
/// is listed.
fn sandbox_lines(ctx: &Ctx, hosts: &[Box<dyn Host>], lines: &mut Vec<String>) -> Result<()> {
    for host in hosts {
        match host.info().kind {
            HostKind::Claude => {
                let text = read_optional(&host.config_file(ctx))?.unwrap_or_default();
                let flag = |key: &str| {
                    jsonc_get(&text, &["sandbox", key]).ok().flatten()
                        == Some(serde_json::Value::Bool(true))
                };
                lines.push(if flag("enabled") && flag("failIfUnavailable") {
                    "Claude Code sandbox: enforced (sandbox.enabled, failIfUnavailable)".to_string()
                } else {
                    "Claude Code sandbox: not enforced; settings.json lacks the sandbox keys"
                        .to_string()
                });
            }
            HostKind::Opencode => {
                let listed = host
                    .plugin_list(ctx)?
                    .iter()
                    .any(|plugin| plugin.name == OPENCODE_PLUGIN);
                lines.push(if listed {
                    format!(
                        "OpenCode sandbox: the relaxkit-opencode plugin is listed; {}",
                        health_line(&plugin_health(ctx))
                    )
                } else {
                    "OpenCode sandbox: the relaxkit-opencode plugin is not listed; run relaxkit setup"
                        .to_string()
                });
            }
        }
    }
    Ok(())
}

/// The savings line of `rtk gain`, or `n/a` when rtk does not report one.
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
            .find(|line| !line.is_empty())
            .unwrap_or("n/a")
            .to_string(),
        _ => "n/a".to_string(),
    }
}
