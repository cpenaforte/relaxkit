//! `relaxkit status` (spec section 6): versions, drift, enforced-vs-advisory settings, the sandbox plugin's health
//! and `rtk gain`. Read-only: it writes nothing and runs only detection and read commands.

use anyhow::Result;

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
use crate::roles::{Role, resolve_claude, resolve_opencode};
use crate::runtime::nvm::{NodeInstall, other_nodes};
use crate::sandbox::opencode::sandbox_config_path;
use crate::sandbox::plugin_health;
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
                        tool.version
                    ));
                    continue;
                };
                let installed = match engine.detect(recipe, host.as_ref()) {
                    Ok(Some((version, _))) => version,
                    Ok(None) => "not found".to_string(),
                    Err(err) => format!("unreadable ({err:#})"),
                };
                let state = if installed == tool.version {
                    "as locked".to_string()
                } else {
                    format!("drift: locked {}", tool.version)
                };
                lines.push(format!(
                    "{id} on {kind}: installed {installed}, locked {} ({}); {state}",
                    tool.version, tool.route
                ));
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
    sandbox_lines(ctx, hosts, lock.as_ref(), &mut lines)?;
    plugin_line(ctx, lock.as_ref(), &mut lines);

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
            match host.info().kind {
                HostKind::Claude => parts.push(match resolve_claude(&setting) {
                    Some(alias) => format!("claude {alias} (advisory)"),
                    None => "claude inherits the host default".to_string(),
                }),
                HostKind::Opencode => parts.push(match resolve_opencode(&setting, &available) {
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
                let expected = roles_block_text(config);
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

/// Whether each host's sandbox is enforcing (spec 7): Claude Code's settings, and the OpenCode sandbox file's mode.
fn sandbox_lines(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    lock: Option<&Lockfile>,
    lines: &mut Vec<String>,
) -> Result<()> {
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
                let Some(tool) = lock.and_then(|lock| lock.tools.get("opencode-sandbox")) else {
                    lines.push("OpenCode sandbox: not installed".to_string());
                    continue;
                };
                let text = read_optional(&sandbox_config_path(ctx))?.unwrap_or_default();
                let mode = jsonc_get(&text, &["mode"]).ok().flatten();
                let enforce = mode.as_ref().and_then(|value| value.as_str()) == Some("enforce");
                lines.push(if enforce {
                    format!(
                        "OpenCode sandbox: opencode-sandbox {} mode enforce (enforced)",
                        tool.version
                    )
                } else {
                    format!(
                        "OpenCode sandbox: opencode-sandbox {} is not in enforce mode",
                        tool.version
                    )
                });
            }
        }
    }
    Ok(())
}

/// The sandbox plugin's release age and runtime lag (spec 10 item 25), or `health unknown` when the registry cannot
/// be read.
fn plugin_line(ctx: &Ctx, lock: Option<&Lockfile>, lines: &mut Vec<String>) {
    let Some(tool) = lock.and_then(|lock| lock.tools.get("opencode-sandbox")) else {
        return;
    };
    lines.push(match plugin_health(ctx, &tool.version) {
        Ok(health) => format!(
            "opencode-sandbox {} (released {}d ago; bundles sandbox-runtime {}, {} releases behind)",
            tool.version, health.release_age_days, health.bundled_runtime, health.runtime_lag
        ),
        Err(err) => format!("opencode-sandbox {} (health unknown: {err:#})", tool.version),
    });
}

/// The savings line of `rtk gain`, or `n/a` when rtk does not report one.
fn rtk_gain(ctx: &Ctx) -> String {
    match ctx.runner.run(&Cmd::new("rtk").arg("gain")) {
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
