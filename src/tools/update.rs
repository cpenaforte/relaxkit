//! The update route of each tool (ruling K71): how one tool moves to a newer exact version on each host it is installed
//! on, and the probe that confirms the move. The caller (`commands::update`) chooses the tools and hosts, takes the
//! backups and the lock, and rolls a tool back when its update or probe fails. Nothing here writes the lock.

use anyhow::{Context, Result, bail};

use crate::exec::Cmd;
use crate::host::{Host, HostCheck, HostKind, check_minimum};
use crate::recipe::{Probe, Recipe, Step, Vars};
use crate::runtime::{chromium, rtk, uv};
use crate::tools::detect::{plugin_name, plugin_spec};
use crate::tools::engine::{Engine, handler, probe_path, refuse_workdir};

/// The hosts a tool moved on, in recipe order. A host that is absent, or too old for the tool, is not in the list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub updated: Vec<HostKind>,
}

/// A copy of `recipe` whose `{version}` is `version`, so every argv, plugin spec and handler sees the new pin.
pub fn pinned(recipe: &Recipe, version: &str) -> Recipe {
    let mut copy = recipe.clone();
    copy.pin = Some(version.to_string());
    copy
}

impl Engine<'_> {
    /// Moves `recipe` to the exact version `target` on every host the recipe names that is installed and new enough,
    /// and runs each host's probe at that version. The first failing step or probe is returned as an error, and the
    /// caller rolls the tool back by calling this again at the locked version.
    pub fn update(&self, recipe: &Recipe, target: &str) -> Result<UpdateOutcome> {
        refuse_workdir(recipe)?;
        let next = pinned(recipe, target);
        let mut targets: Vec<(HostKind, &dyn Host)> = Vec::new();
        for &kind in &recipe.hosts {
            let Some(host) = self.hosts.iter().find(|host| host.info().kind == kind) else {
                continue;
            };
            if let Some(need) = recipe.minimum_host.get(&kind)
                && let HostCheck::TooOld { .. } = check_minimum(host.info(), need)
            {
                continue;
            }
            targets.push((kind, host.as_ref()));
        }
        let Some(&(_, first)) = targets.first() else {
            return Ok(UpdateOutcome::default());
        };
        self.update_once(&next, target, first)?;
        let mut updated = Vec::new();
        for (kind, host) in targets {
            self.update_on_host(&next, kind, host)?;
            self.probe(&next, kind)?;
            updated.push(kind);
        }
        Ok(UpdateOutcome { updated })
    }

    /// The steps that run once per tool, not once per host: a binary download, a Chromium build, an npm CLI, a uv
    /// tool. `host` carries the command's log and config guard.
    fn update_once(&self, next: &Recipe, target: &str, host: &dyn Host) -> Result<()> {
        match next.id.as_str() {
            "rtk" => {
                // rtk is always the latest release (owner's exception, ruling R2-2). The release written must be the
                // one the update asked for, or the tool is not confirmed.
                let tag = rtk::reinstall_latest(self.ctx, self.run)?;
                let bare = tag.strip_prefix('v').unwrap_or(&tag);
                if bare != target {
                    bail!("rtk's latest release is {tag}, not {target}, so nothing was confirmed");
                }
            }
            "playwright" => chromium::ensure_chromium(self.ctx, self.run, target)?,
            "serena" => {
                // `uv tool install` at the newest pin, with --force so an installed tool moves (spike I6). The pin is
                // written with `==`, the requirement form uv reads; `@` would name a URL.
                uv::ensure_uv(self.ctx, self.run)?;
                let argv = [
                    "uv".to_string(),
                    "tool".to_string(),
                    "install".to_string(),
                    "-p".to_string(),
                    "3.13".to_string(),
                    format!("serena-agent=={target}"),
                    "--force".to_string(),
                ];
                let cmd = self.command(&argv)?;
                self.run_checked(host, &cmd)?;
            }
            "caveman" => {
                // The CLI is npm-installed at the newest pin, through the handler the install path also uses.
                let run = handler("caveman_cli").context("caveman_cli handler is missing")?;
                let mut vars = self.vars(next)?;
                run(self, next, host, &mut vars)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The steps that run on one host: an MCP entry whose argv carries the version is removed and added again, a
    /// plugin is updated through its host, and the host's configuration steps run again.
    fn update_on_host(&self, next: &Recipe, kind: HostKind, host: &dyn Host) -> Result<()> {
        let steps: Vec<Step> = next.install.get(&kind).cloned().unwrap_or_default();
        for step in &steps {
            if let Step::Mcp { name, argv } = step
                && argv.iter().any(|arg| arg.contains("{version}"))
            {
                host.mcp_remove(self.ctx, name)?;
                let vars = self.vars(next)?;
                self.add_mcp(next, host, name, argv, &vars)?;
            }
        }
        if let Some(spec) = plugin_spec(next, kind) {
            match kind {
                HostKind::Claude => self.claude_plugin_update(next, host, spec)?,
                HostKind::Opencode => {
                    let vars = self.vars(next)?;
                    let expanded = next.expand(&[spec.to_string()], &vars)?;
                    host.plugin_update(self.ctx, &expanded[0])?;
                    // The config must list the new spec before the lock may move (K67, R4-2): an exit of 0 from
                    // `opencode plugin add` is not proof that the plugin is now at the target version.
                    let name = plugin_name(&expanded[0]).to_string();
                    let want = next.pin.clone().unwrap_or_default();
                    let listed = host.plugin_list(self.ctx)?;
                    // Exactly one entry for the package, at the target version (F2-4). Two entries means the old spec
                    // survived the remove, and two loaded copies of one plugin are not the target state.
                    let entries: Vec<_> =
                        listed.iter().filter(|plugin| plugin.name == name).collect();
                    if entries.len() > 1 {
                        bail!(
                            "the OpenCode config lists {} entries for {name}, not one, after the update",
                            entries.len()
                        );
                    }
                    if !entries
                        .iter()
                        .any(|plugin| plugin.version.as_deref() == Some(want.as_str()))
                    {
                        bail!(
                            "the OpenCode config does not list {name} at {want} after the update"
                        );
                    }
                }
            }
        }
        if next.id == "caveman" && kind == HostKind::Opencode {
            // caveman's OpenCode files come from its newest tag (spec 5.1), through the same handler as the install.
            let run = handler("caveman_opencode").context("caveman_opencode handler is missing")?;
            let mut vars = self.vars(next)?;
            run(self, next, host, &mut vars)?;
        }
        if next.id == "serena" {
            let run = handler("serena_init").context("serena_init handler is missing")?;
            let mut vars = self.vars(next)?;
            run(self, next, host, &mut vars)?;
        }
        // The configuration steps run again after the change, so a new binary keeps its hook and its settings (rtk's
        // hook, caveman's routing and telemetry).
        self.run_config_steps(next, host)?;
        Ok(())
    }

    /// Updates a Claude Code plugin by name at its installed marketplace. Every marketplace is refreshed first, except
    /// for caveman, whose plugin is updated by name only (K71).
    fn claude_plugin_update(&self, next: &Recipe, host: &dyn Host, spec: &str) -> Result<()> {
        let name = plugin_name(spec).to_string();
        if next.id != "caveman" {
            host.marketplace_update(self.ctx)?;
        }
        let listed = host
            .plugin_list(self.ctx)?
            .into_iter()
            .find(|plugin| plugin.name == name)
            .and_then(|plugin| plugin.marketplace);
        let marketplace = match listed {
            Some(marketplace) => marketplace,
            None => match spec.split_once('@') {
                Some((_, marketplace)) if !marketplace.contains('{') => marketplace.to_string(),
                _ => bail!(
                    "`{name}` is not installed from a known marketplace, so it cannot be updated"
                ),
            },
        };
        host.plugin_update(self.ctx, &format!("{name}@{marketplace}"))
    }

    /// Runs the recipe's probe for `kind` at the pinned version. The probe must exit 0, and its output must hold the
    /// recipe's `expect` text, or the plugin's name for a plugin listing. A probe that cannot run fails the check.
    fn probe(&self, next: &Recipe, kind: HostKind) -> Result<()> {
        let Some(probe) = next.probe.get(&kind) else {
            return Ok(());
        };
        let vars: Vars = self.vars(next)?;
        let argv = next.expand(&probe.argv, &vars)?;
        let Some((program, args)) = argv.split_first() else {
            bail!("recipe `{}` has an empty probe for {kind}", next.id);
        };
        let shown = argv.join(" ");
        // A bare program resolves the way doctor resolves it: the pack copy first, then the PATH.
        let resolved = self
            .probe_on_path(next, kind)
            .map(|(_, path)| path.display().to_string())
            .unwrap_or_else(|| program.clone());
        let cmd = args
            .iter()
            .fold(Cmd::new(&resolved), |cmd, arg| cmd.arg(arg))
            .env("PATH", &probe_path(self.ctx));
        let out = self
            .ctx
            .runner
            .run(&cmd)
            .with_context(|| format!("probe `{shown}` did not run"))?;
        if out.code != 0 {
            bail!("probe `{shown}` exited {}", out.code);
        }
        if let Some(text) = expected_text(next, kind, probe)
            && !out.stdout.contains(&text)
            && !out.stderr.contains(&text)
        {
            bail!("probe `{shown}` does not show {text}");
        }
        Ok(())
    }
}

/// The text a passing probe's output must hold: the recipe's `expect`, or, for a plugin listing with none, the
/// plugin's name for this host.
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
