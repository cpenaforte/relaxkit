//! The install engine. For each tool and host it checks the host's version, detects an existing install (adopt,
//! do not reinstall), runs the recipe's install steps at the pinned version, and records the result in the lock.
//! Install steps run in order and stop at the first failure. A tool whose steps fail is not locked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::exec::{Cmd, Output};
use crate::host::{Host, HostCheck, HostKind, check_minimum, edit_guarded, parse_version};
use crate::net::{Net, github_latest_tag, npm_latest};
use crate::platform::is_executable;
use crate::recipe::{Kind, Recipe, Step, Vars, VersionSource};
use crate::runtime::nvm::{BREW_PREFIXES, NodeInstall, node_bin, npx_bin};
use crate::runtime::{chromium, rtk, utf8, uv};
use crate::state::RunId;
use crate::state::lockfile::{LockedTool, Lockfile, check_version};
use crate::tools::detect::{Route, install_route, plugin_name, plugin_spec, uses_uv};

/// The version recorded for a tool that has no version source (a plugin that the host versions itself). Such a
/// tool is not written to the lock, because the lock stores only exact versions.
pub const UNVERSIONED: &str = "unversioned";

/// A named code path a recipe may run with a `handler` step. It receives the host and the variables that later
/// steps expand with, so it can hand a value (a marketplace, a Chromium path) to them.
pub type HandlerFn = for<'a> fn(&Engine<'a>, &Recipe, &dyn Host, &mut Vars) -> Result<()>;

/// The handler registry: the code path a handler name runs, or `None` when this build does not implement it.
pub fn handler(name: &str) -> Option<HandlerFn> {
    crate::tools::handlers::lookup(name)
}

/// What the engine did for one tool on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Installed { version: String },
    Adopted { version: String, route: Route },
    Skipped { reason: String },
}

/// A host and its plan, or `None` when the host is not installed.
type Planned<'a> = (HostKind, Option<(&'a dyn Host, Plan)>);

/// What the engine will do on one host, decided before any host runs a step.
enum Plan {
    Skipped(String),
    Adopt { version: String, route: Route },
    Install,
}

impl Plan {
    /// The steps that will run on `kind`: the configuration steps of an adopted install, every step of an install.
    fn steps(&self, recipe: &Recipe, kind: HostKind) -> Vec<Step> {
        match self {
            Plan::Skipped(_) => Vec::new(),
            Plan::Adopt { .. } => config_steps(recipe, kind),
            Plan::Install => recipe.install.get(&kind).cloned().unwrap_or_default(),
        }
    }
}

/// Handlers that only configure a tool that is already installed. An adopted install runs them too (ruling T12-2
/// covers configuration steps), so rtk on Claude gets its hook whether relaxkit installed rtk or found it.
const CONFIGURATION_HANDLERS: [&str; 2] = ["rtk_claude_hook", "opencode_sandbox_config"];

/// The steps of a recipe on `kind` that install or add nothing: the configuration an adopted install still
/// needs (for example `rtk init -g`, `caveman telemetry off` or the rtk hook).
fn config_steps(recipe: &Recipe, kind: HostKind) -> Vec<Step> {
    recipe
        .install
        .get(&kind)
        .into_iter()
        .flatten()
        .filter(|step| match step {
            Step::Exec { argv, .. } => !argv
                .iter()
                .any(|token| token == "install" || token == "add"),
            Step::Handler { name } => CONFIGURATION_HANDLERS.contains(&name.as_str()),
            _ => false,
        })
        .cloned()
        .collect()
}

pub struct Engine<'a> {
    pub ctx: &'a Ctx,
    pub run: &'a RunId,
    pub hosts: &'a [Box<dyn Host>],
    pub recipes: &'a [Recipe],
    pub node: &'a NodeInstall,
}

impl<'a> Engine<'a> {
    /// The newest release of the tool, from its version source, as a bare exact version.
    pub fn latest_version(&self, recipe: &Recipe) -> Result<String> {
        let net: &dyn Net = self.ctx.net.as_ref();
        let raw = match &recipe.version {
            VersionSource::Github { repo } => github_latest_tag(net, repo)?,
            VersionSource::Npm { package } => npm_latest(net, package)?,
            VersionSource::Pypi { package } => pypi_latest(net, package)?,
            VersionSource::None => bail!("recipe `{}` has no version source", recipe.id),
        };
        let bare = raw.strip_prefix('v').unwrap_or(&raw).to_string();
        check_version(&bare).with_context(|| format!("recipe `{}`", recipe.id))?;
        Ok(bare)
    }

    /// An existing install of the tool on one host, with its version and route. `None` means install it.
    pub fn detect(&self, recipe: &Recipe, host: &dyn Host) -> Result<Option<(String, Route)>> {
        let kind = host.info().kind;
        if let Some(spec) = plugin_spec(recipe, kind) {
            let name = plugin_name(spec);
            let Some(found) = host
                .plugin_list(self.ctx)?
                .into_iter()
                .find(|plugin| plugin.name == name)
            else {
                return Ok(None);
            };
            let route = match kind {
                HostKind::Claude => {
                    Route::ClaudePlugin(found.marketplace.clone().unwrap_or_default())
                }
                HostKind::Opencode => Route::OpencodePlugin,
            };
            // The pin is recorded only when the spec pins it. Otherwise the host's reported version is the record.
            let pinned = spec
                .contains("{version}")
                .then(|| recipe.pin.clone())
                .flatten();
            let version = found
                .version
                .or(pinned)
                .unwrap_or_else(|| UNVERSIONED.to_string());
            return Ok(Some((version, route)));
        }
        // An MCP entry is always written again, because the entry may be missing or different.
        if recipe.kind == Kind::Mcp {
            return Ok(None);
        }
        // OpenCode wiring is read from the host's plugin list (its config), never from a binary on the PATH.
        if kind == HostKind::Opencode {
            let Some(found) = host
                .plugin_list(self.ctx)?
                .into_iter()
                .find(|plugin| plugin.name == recipe.id)
            else {
                return Ok(None);
            };
            // A pinned npm or binary tool keeps its pin when the config reports no version. Only a plugin spec with no
            // pin of its own is left unversioned.
            let version = found
                .version
                .or_else(|| recipe.pin.clone())
                .unwrap_or_else(|| UNVERSIONED.to_string());
            let route = match recipe.kind {
                Kind::Npm => Route::Npm,
                Kind::Binary => Route::Script,
                _ => Route::Unknown,
            };
            return Ok(Some((version, route)));
        }
        self.probed_binary(recipe, kind)
    }

    /// The probe's first word, when that binary is on the PATH: its name and the path it resolves to. A copy relaxkit
    /// installed in its bin directory counts too, because the engine's commands search that directory. Such a copy
    /// is probed by its full path, since the bare name is not on the PATH.
    fn probe_on_path(&self, recipe: &Recipe, kind: HostKind) -> Option<(String, PathBuf)> {
        let probe = recipe.probe.get(&kind)?;
        let bin = probe.argv.first().filter(|arg| !arg.starts_with('{'))?;
        // rtk resolves its pack copy first, so a broken rtk elsewhere on the PATH does not decide (ruling R3-2).
        if recipe.id == "rtk"
            && let Some(path) = rtk::resolve(self.ctx)
        {
            return Some((path.to_string_lossy().into_owned(), path));
        }
        if let Some(path) = self.ctx.runner.which(bin) {
            return Some((bin.clone(), path));
        }
        if bin.contains('/') {
            return None;
        }
        let own = self.ctx.dirs.bin.join(bin);
        is_executable(&own).then(|| (own.to_string_lossy().into_owned(), own))
    }

    /// The version a binary reports for `--version`, when it reports one.
    fn reported_version(&self, bin: &str) -> Option<String> {
        self.ctx
            .runner
            .run(&Cmd::new(bin).arg("--version"))
            .ok()
            .filter(|out| out.code == 0)
            .and_then(|out| parse_version(&out.stdout))
    }

    /// The version and route of the recipe's binary when it is on the PATH and reports a version. Nothing is returned
    /// for a missing or silent binary.
    fn probed_binary(&self, recipe: &Recipe, kind: HostKind) -> Result<Option<(String, Route)>> {
        let Some((bin, path)) = self.probe_on_path(recipe, kind) else {
            return Ok(None);
        };
        let Some(version) = self.reported_version(&bin) else {
            return Ok(None);
        };
        Ok(Some((version, binary_route(recipe, &path))))
    }

    /// Installs the recipe on every host it names. A host that is not installed, or is too old, is skipped. An
    /// existing install is adopted: its configuration steps run and no install step does. Every installed host's
    /// steps are checked before the first host runs anything. The lock is written only when every installed host
    /// succeeded.
    pub fn install(&self, recipe: &Recipe) -> Result<Vec<(HostKind, Outcome)>> {
        refuse_workdir(recipe)?;
        let mut planned: Vec<Planned<'_>> = Vec::new();
        for &kind in &recipe.hosts {
            let entry = match self.hosts.iter().find(|host| host.info().kind == kind) {
                None => None,
                Some(host) => Some((host.as_ref(), self.plan(recipe, host.as_ref())?)),
            };
            planned.push((kind, entry));
        }
        for (kind, entry) in &planned {
            if let Some((_, plan)) = entry {
                let steps = plan.steps(recipe, *kind);
                self.preflight_steps(recipe, *kind, &steps)?;
            }
        }
        let mut outcomes = Vec::new();
        let mut rows: Vec<(HostKind, String, Route)> = Vec::new();
        for (kind, entry) in planned {
            let Some((host, plan)) = entry else {
                outcomes.push((
                    kind,
                    Outcome::Skipped {
                        reason: format!("{kind} is not installed"),
                    },
                ));
                continue;
            };
            match plan {
                Plan::Skipped(reason) => outcomes.push((kind, Outcome::Skipped { reason })),
                Plan::Adopt { version, route } => {
                    self.run_config_steps(recipe, host)?;
                    rows.push((kind, version.clone(), route.clone()));
                    outcomes.push((kind, Outcome::Adopted { version, route }));
                }
                Plan::Install => {
                    let (version, route) = self.install_here(recipe, kind, host)?;
                    rows.push((kind, version.clone(), route.clone()));
                    outcomes.push((kind, Outcome::Installed { version }));
                }
            }
        }
        self.record(recipe, &rows)?;
        Ok(outcomes)
    }

    /// Runs every install step of the recipe on one host, and returns the version and route the lock records for it.
    fn install_here(
        &self,
        recipe: &Recipe,
        kind: HostKind,
        host: &dyn Host,
    ) -> Result<(String, Route)> {
        let vars = self.run_steps(recipe, host)?;
        // A binary or npm tool records the version its binary reports after the steps ran, and that binary's route.
        // Only when nothing is reported does it fall back to the pin or the plugin rule.
        match self.binary_after_steps(recipe, kind)? {
            Some(found) => Ok(found),
            None => {
                let version = self.installed_version(recipe, host)?;
                let marketplace = plugin_marketplace(recipe, kind, &vars);
                Ok((version, install_route(recipe, kind, &marketplace)))
            }
        }
    }

    /// Runs the recipe's install steps again on one installed host, at the recipe's pin, and updates the lock for that
    /// host. Unlike `install`, an adopted tool is not skipped, and the steps include the MCP entry, which the
    /// configuration of an adopted install does not rerun. Setup uses this to repair a failing check (ruling OWNER-3).
    pub fn reinstall(&self, recipe: &Recipe, kind: HostKind) -> Result<Outcome> {
        refuse_workdir(recipe)?;
        let Some(host) = self.hosts.iter().find(|host| host.info().kind == kind) else {
            bail!(
                "{kind} is not installed, so {} cannot be reinstalled on it",
                recipe.id
            );
        };
        let steps = recipe.install.get(&kind).cloned().unwrap_or_default();
        self.preflight_steps(recipe, kind, &steps)?;
        // An unpinned plugin that is already listed keeps the version it has: only its configuration runs again, and
        // `claude plugin install` is never called, because it would move the version (ruling R2-3).
        let listed = match plugin_spec(recipe, kind) {
            Some(spec) if !spec.contains("{version}") => self.detect(recipe, host.as_ref())?,
            _ => None,
        };
        if let Some((version, route)) = listed {
            self.run_config_steps(recipe, host.as_ref())?;
            self.record(recipe, &[(kind, version.clone(), route.clone())])?;
            return Ok(Outcome::Adopted { version, route });
        }
        // rtk is always the latest release (owner's exception, ruling R2-2), so its repair installs that release even
        // when rtk is already on the PATH, and the release tag is what the lock records.
        let latest = if recipe.id == "rtk" {
            Some(
                rtk::reinstall_latest(self.ctx, self.run)?
                    .trim_start_matches('v')
                    .to_string(),
            )
        } else {
            None
        };
        let (installed, route) = self.install_here(recipe, kind, host.as_ref())?;
        // The rtk repair records the release it installed, as a script copy in the pack bin (ruling R3-2).
        let (version, route) = match latest {
            Some(tag) => (tag, Route::Script),
            None => (installed, route),
        };
        self.record(recipe, &[(kind, version.clone(), route)])?;
        Ok(Outcome::Installed { version })
    }

    /// The version an install records. A plugin step that carries `{version}` is pinned, so the pin is recorded. A
    /// plugin step without it is installed at whatever the marketplace serves, so the version the host reports
    /// after the install is recorded, and `UNVERSIONED` when it reports none.
    fn installed_version(&self, recipe: &Recipe, host: &dyn Host) -> Result<String> {
        let kind = host.info().kind;
        if let Some(spec) = plugin_spec(recipe, kind)
            && !spec.contains("{version}")
        {
            let name = plugin_name(spec);
            let reported = host
                .plugin_list(self.ctx)?
                .into_iter()
                .find(|plugin| plugin.name == name)
                .and_then(|plugin| plugin.version);
            return Ok(reported.unwrap_or_else(|| UNVERSIONED.to_string()));
        }
        Ok(recipe
            .pin
            .clone()
            .unwrap_or_else(|| UNVERSIONED.to_string()))
    }

    fn binary_after_steps(
        &self,
        recipe: &Recipe,
        kind: HostKind,
    ) -> Result<Option<(String, Route)>> {
        if !matches!(recipe.kind, Kind::Binary | Kind::Npm) || plugin_spec(recipe, kind).is_some() {
            return Ok(None);
        }
        // A binary that is on the PATH but reports no version is recorded as unversioned, never as the pin.
        let Some((bin, path)) = self.probe_on_path(recipe, kind) else {
            return Ok(None);
        };
        let version = self
            .reported_version(&bin)
            .unwrap_or_else(|| UNVERSIONED.to_string());
        Ok(Some((version, binary_route(recipe, &path))))
    }

    /// Decides what happens on one host without changing anything: skip, adopt, or install.
    fn plan(&self, recipe: &Recipe, host: &dyn Host) -> Result<Plan> {
        let info = host.info();
        if let Some(need) = recipe.minimum_host.get(&info.kind)
            && let HostCheck::TooOld { have, need } = check_minimum(info, need)
        {
            let reason = format!(
                "{} {have} is older than {need}, the oldest this tool supports",
                info.kind
            );
            return Ok(Plan::Skipped(reason));
        }
        match self.detect(recipe, host)? {
            Some((version, route)) => Ok(Plan::Adopt { version, route }),
            None => Ok(Plan::Install),
        }
    }

    /// Every handler a step list names must exist before anything runs, so an unimplemented handler stops the
    /// install before a command is run.
    fn preflight_steps(&self, recipe: &Recipe, kind: HostKind, steps: &[Step]) -> Result<()> {
        for step in steps {
            if let Step::Handler { name } = step
                && handler(name).is_none()
            {
                bail!(
                    "handler `{name}` is not implemented in this build, so recipe `{}` cannot be installed on {kind}; nothing was run",
                    recipe.id
                );
            }
        }
        Ok(())
    }

    /// Runs the configuration steps of an adopted install, in order: the exec steps that do not install or add
    /// anything, and the configuration handlers.
    fn run_config_steps(&self, recipe: &Recipe, host: &dyn Host) -> Result<Vars> {
        let kind = host.info().kind;
        let mut vars = self.vars(recipe)?;
        for step in config_steps(recipe, kind) {
            match step {
                Step::Exec { argv, .. } => {
                    let argv = recipe.expand(&argv, &vars)?;
                    let cmd = self.command(&argv)?;
                    self.run_checked(host, &cmd)?;
                }
                Step::Handler { name } => {
                    let run = handler(&name).ok_or_else(|| {
                        anyhow!("handler `{name}` is not implemented in this build")
                    })?;
                    run(self, recipe, host, &mut vars)?;
                }
                _ => {}
            }
        }
        Ok(vars)
    }

    fn run_steps(&self, recipe: &Recipe, host: &dyn Host) -> Result<Vars> {
        let kind = host.info().kind;
        let steps: &[Step] = recipe.install.get(&kind).map(Vec::as_slice).unwrap_or(&[]);
        if recipe.id == "rtk" {
            rtk::ensure_rtk(self.ctx, self.run)?;
        }
        if uses_uv(steps) {
            uv::ensure_uv(self.ctx, self.run)?;
        }
        let mut vars = self.vars(recipe)?;
        for step in steps {
            match step {
                Step::Exec { argv, .. } => {
                    let argv = recipe.expand(argv, &vars)?;
                    let cmd = self.command(&argv)?;
                    self.run_checked(host, &cmd)?;
                }
                Step::Mcp { name, argv } => self.add_mcp(recipe, host, name, argv, &vars)?,
                Step::Plugin { spec } => {
                    let spec = expand_one(recipe, spec, &vars)?;
                    host.plugin_install(self.ctx, &spec)?;
                }
                Step::Handler { name } => {
                    let run = handler(name).ok_or_else(|| {
                        anyhow!("handler `{name}` is not implemented in this build")
                    })?;
                    run(self, recipe, host, &mut vars)?;
                }
            }
        }
        Ok(vars)
    }

    /// Adds one MCP entry through the host. The entry starts Node through the fixed link (spec 4, items 4 and 5), so PATH
    /// names that link first, then the system directories, and no other PATH is inherited.
    fn add_mcp(
        &self,
        recipe: &Recipe,
        host: &dyn Host,
        name: &str,
        argv: &[String],
        vars: &Vars,
    ) -> Result<()> {
        let argv = recipe.expand(argv, vars)?;
        let mut env = vec![("PATH".to_string(), self.mcp_path())];
        env.extend(
            recipe
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        host.mcp_add(self.ctx, name, &argv, &env)
    }

    /// Adds one locked MCP entry again through the host's MCP step, without reinstalling its tool (ruling R2-1).
    pub fn readd_mcp(&self, recipe: &Recipe, kind: HostKind) -> Result<()> {
        let Some(host) = self.hosts.iter().find(|host| host.info().kind == kind) else {
            bail!(
                "{kind} is not installed, so {} cannot be re-added on it",
                recipe.id
            );
        };
        let entry = recipe
            .install
            .get(&kind)
            .into_iter()
            .flatten()
            .find_map(|step| match step {
                Step::Mcp { name, argv } => Some((name, argv)),
                _ => None,
            });
        let Some((name, argv)) = entry else {
            bail!("{} has no MCP entry on {kind}", recipe.id);
        };
        let vars = self.vars(recipe)?;
        self.add_mcp(recipe, host.as_ref(), name, argv, &vars)
    }

    /// The variables steps expand with. Node and npx come from the fixed link; Chromium is resolved only when a
    /// step names it.
    fn vars(&self, recipe: &Recipe) -> Result<Vars> {
        let mut vars = Vars {
            node: utf8(&node_bin(self.ctx))?.to_string(),
            npx: utf8(&npx_bin(self.ctx))?.to_string(),
            marketplace: String::new(),
            chromium: String::new(),
        };
        if mentions(recipe, "{chromium}") {
            vars.chromium = self.chromium_location()?;
        }
        Ok(vars)
    }

    /// The Chromium that Playwright installed. A dry run that cannot find it uses a placeholder, because nothing
    /// is run in a dry run.
    pub(crate) fn chromium_location(&self) -> Result<String> {
        match chromium::installed_chromium(self.ctx) {
            Ok(path) => Ok(utf8(&path)?.to_string()),
            Err(_) if self.ctx.dry_run => Ok("<chromium>".to_string()),
            Err(err) => Err(err),
        }
    }

    /// A command with the Node link, the pack bin and `~/.local/bin` first on PATH, so a tool installed a moment
    /// earlier resolves by its bare name.
    pub(crate) fn command(&self, argv: &[String]) -> Result<Cmd> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| anyhow!("an empty command cannot run"))?;
        let mut cmd = Cmd::new(program);
        for arg in args {
            cmd = cmd.arg(arg);
        }
        Ok(cmd.env("PATH", &self.search_path()?))
    }

    /// The PATH an MCP entry runs with, as `mcp_path_dirs` lists it.
    fn mcp_path(&self) -> String {
        mcp_path_dirs(self.ctx)
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }

    fn search_path(&self) -> Result<String> {
        let mut dirs = vec![
            utf8(&self.node.root.join("bin"))?.to_string(),
            utf8(&self.ctx.dirs.bin)?.to_string(),
            utf8(&self.ctx.dirs.home.join(".local").join("bin"))?.to_string(),
        ];
        if let Some(current) = std::env::var_os("PATH") {
            dirs.push(current.to_string_lossy().into_owned());
        }
        Ok(dirs.join(":"))
    }

    /// Runs a host command. The host's config and instructions files, when they exist, are backed up and
    /// journaled first, because a command may rewrite them.
    pub(crate) fn run_checked(&self, host: &dyn Host, cmd: &Cmd) -> Result<()> {
        let files: Vec<PathBuf> = [host.config_file(self.ctx), host.instructions_file(self.ctx)]
            .into_iter()
            .filter(|path| path.exists())
            .collect();
        let out: Output = edit_guarded(self.ctx, &files, cmd)?;
        if out.code != 0 {
            bail!(
                "`{cmd}` failed with exit {}: {}",
                out.code,
                out.stderr.trim()
            );
        }
        Ok(())
    }

    /// Writes the lock entry for a tool whose hosts all installed or adopted. A tool whose version is not exact
    /// (for example a plugin with no version source) is not locked.
    fn record(&self, recipe: &Recipe, rows: &[(HostKind, String, Route)]) -> Result<()> {
        if rows.is_empty() || self.ctx.dry_run {
            return Ok(());
        }
        let (kind, version, _) = &rows[0];
        if check_version(version).is_err() {
            self.ctx.out.info(&format!(
                "{} on {kind} is not locked: the host reports no exact version",
                recipe.id
            ));
            return Ok(());
        }
        let first = &rows[0].2;
        let route = if rows.iter().all(|(_, _, route)| route == first) {
            first.as_str()
        } else {
            rows.iter()
                .map(|(kind, _, route)| format!("{kind}:{}", route.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut lock = Lockfile::load(self.ctx)?.unwrap_or_else(|| Lockfile {
            relaxkit: env!("CARGO_PKG_VERSION").to_string(),
            tools: BTreeMap::new(),
        });
        // The hosts already locked for this tool stay locked: a run on some hosts only adds to the list (ruling K56).
        let mut hosts: BTreeSet<String> =
            rows.iter().map(|(kind, _, _)| kind.to_string()).collect();
        if let Some(existing) = lock.tools.get(&recipe.id) {
            hosts.extend(existing.hosts.iter().cloned());
        }
        lock.tools.insert(
            recipe.id.clone(),
            LockedTool {
                version: version.clone(),
                route,
                hosts: hosts.into_iter().collect(),
            },
        );
        lock.save(self.ctx, self.run)
    }
}

/// A recipe that sets a workdir cannot be applied through the Host API (SA-1). Until the owner decides how it is
/// applied, the install is refused before any step, so the process never starts without it.
fn refuse_workdir(recipe: &Recipe) -> Result<()> {
    if recipe.workdir.is_some() {
        bail!(
            "workdir not applied (SA-1, owner decision pending): {}",
            recipe.id
        );
    }
    Ok(())
}

/// The directories an MCP entry's command is found in, in order: the Node link, then `~/.local/bin` (where
/// `uv tool install` puts `serena`), then the system directories. No other PATH is inherited (spec 4, items 4 and 5).
pub fn mcp_path_dirs(ctx: &Ctx) -> Vec<PathBuf> {
    vec![
        ctx.dirs.data.join("node").join("bin"),
        ctx.dirs.home.join(".local").join("bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]
}

/// The PATH a probe child runs with: the Node link bin, the pack bin, `~/.local/bin`, then the PATH doctor was started
/// with (ruling R2-4). It is the engine's search order, so a probe finds the tools the engine installed.
pub fn probe_path(ctx: &Ctx) -> String {
    let mut entries = vec![
        ctx.dirs.data.join("node").join("bin").display().to_string(),
        ctx.dirs.bin.display().to_string(),
        ctx.dirs
            .home
            .join(".local")
            .join("bin")
            .display()
            .to_string(),
    ];
    if let Some(current) = std::env::var_os("PATH") {
        entries.push(current.to_string_lossy().into_owned());
    }
    entries.join(":")
}

/// The route of a binary or npm recipe found at `path`: Brew under a Homebrew prefix, otherwise by recipe kind.
fn binary_route(recipe: &Recipe, path: &std::path::Path) -> Route {
    if BREW_PREFIXES.iter().any(|prefix| path.starts_with(prefix)) {
        return Route::Brew;
    }
    match recipe.kind {
        Kind::Npm => Route::Npm,
        Kind::Binary => Route::Script,
        _ => Route::Unknown,
    }
}

/// The marketplace a Claude plugin step installs from: the resolved `{marketplace}`, or the part after `@` in a
/// plain spec. Empty for anything else.
fn plugin_marketplace(recipe: &Recipe, kind: HostKind, vars: &Vars) -> String {
    match plugin_spec(recipe, kind) {
        Some(spec) if spec.contains("{marketplace}") => vars.marketplace.clone(),
        Some(spec) => spec
            .split_once('@')
            .map(|(_, marketplace)| marketplace.to_string())
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Expands one string with the recipe's placeholders. A `{marketplace}` that is not resolved is an error, so no
/// spec is ever built with an empty marketplace.
fn expand_one(recipe: &Recipe, text: &str, vars: &Vars) -> Result<String> {
    if text.contains("{marketplace}") && vars.marketplace.is_empty() {
        bail!(
            "recipe `{}` needs a marketplace that no handler resolved",
            recipe.id
        );
    }
    let mut expanded = recipe.expand(&[text.to_string()], vars)?;
    Ok(expanded.remove(0))
}

/// Whether any step or probe of the recipe contains `needle`.
fn mentions(recipe: &Recipe, needle: &str) -> bool {
    let steps = recipe.install.values().flatten().any(|step| match step {
        Step::Exec { argv, .. } | Step::Mcp { argv, .. } => argv.iter().any(|a| a.contains(needle)),
        Step::Plugin { spec } => spec.contains(needle),
        Step::Handler { .. } => false,
    });
    steps
        || recipe
            .probe
            .values()
            .any(|probe| probe.argv.iter().any(|a| a.contains(needle)))
}

/// Latest version of a PyPI project.
fn pypi_latest(net: &dyn Net, package: &str) -> Result<String> {
    let body = net.get_text(&format!("https://pypi.org/pypi/{package}/json"))?;
    let value: Value = serde_json::from_str(&body).context("PyPI response is not JSON")?;
    value
        .pointer("/info/version")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("PyPI response for `{package}` has no version"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::host::HostInfo;
    use crate::host::claude::ClaudeHost;
    use crate::host::opencode::OpencodeHost;
    use crate::net::FakeNet;
    use crate::recipe::{load_all, parse_recipe};
    use crate::runtime::nvm::npx_bin;
    use crate::runtime::testing::{Recorder, ctx_with};
    use crate::state::lockfile::{LockedTool, Lockfile};

    const MCP_FIXTURE: &str = r#"
id = "fixture"
title = "Fixture"
kind = "mcp"
hosts = ["claude"]
needs_login = false
telemetry_default_off = true

[version]
npm = "@playwright/mcp"
pin = "0.0.86"

[minimum_host]
claude = "2.1.330"

[probe.claude]
argv = ["{npx}", "-y", "@playwright/mcp@{version}", "--help"]

[[install.claude]]
mcp = "playwright"
argv = ["{npx}", "-y", "@playwright/mcp@{version}", "--browser", "chromium", "--isolated"]
"#;

    const PLUGIN_FIXTURE: &str = r#"
id = "fixture"
title = "Fixture"
kind = "plugin"
hosts = ["claude"]
needs_login = false
telemetry_default_off = true

[version]
github = "obra/superpowers"
pin = "7.0.2"

[minimum_host]
claude = "2.1.330"

[probe.claude]
argv = ["claude", "plugin", "list"]
expect = "superpowers"

[[install.claude]]
plugin = "superpowers@anthropic-plugin-directory"
"#;

    /// A plugin with no `{version}` in its spec, installed on both hosts.
    const PLUGIN_ON_BOTH_HOSTS: &str = r#"
id = "fixture"
title = "Fixture"
kind = "plugin"
hosts = ["claude", "opencode"]
needs_login = false
telemetry_default_off = true

[version]
github = "obra/superpowers"
pin = "7.0.2"

[minimum_host]
claude = "2.1.330"
opencode = "2.2.6"

[probe.claude]
argv = ["claude", "plugin", "list"]
expect = "superpowers"

[probe.opencode]
argv = ["opencode", "--version"]
expect = "superpowers"

[[install.claude]]
plugin = "superpowers@anthropic-plugin-directory"

[[install.opencode]]
plugin = "superpowers"
"#;

    const EXEC_FIXTURE: &str = r#"
id = "fixture"
title = "Fixture"
kind = "npm"
hosts = ["claude"]
needs_login = false
telemetry_default_off = true

[version]
npm = "fixture-pkg"
pin = "1.2.3"

[minimum_host]
claude = "2.1.330"

[probe.claude]
argv = ["fixture-pkg", "--help"]

[[install.claude]]
exec = ["fixture-pkg", "install"]

[[install.claude]]
exec = ["rtk", "telemetry", "disable"]

[[install.claude]]
exec = ["caveman", "telemetry", "off"]
"#;

    fn fixture(text: &str) -> Recipe {
        parse_recipe(text).expect("fixture parses")
    }

    /// Points every handler step at a name the registry does not hold, so the unimplemented-handler path runs. The
    /// parser accepts only names in `KNOWN_HANDLERS`, and every one of those is implemented now.
    fn unregistered_handler(recipe: &mut Recipe) {
        for steps in recipe.install.values_mut() {
            for step in steps.iter_mut() {
                if let Step::Handler { name } = step {
                    *name = "not_yet_built".to_string();
                }
            }
        }
    }

    fn node(root: &Path) -> NodeInstall {
        NodeInstall {
            version: "v24.22.3".to_string(),
            root: root.join(".local/share/relaxkit/node/versions/node/v24.22.3"),
        }
    }

    fn claude(root: &Path, version: &str) -> Box<dyn Host> {
        let info = HostInfo {
            kind: HostKind::Claude,
            binary: PathBuf::from("/usr/local/bin/claude"),
            version: version.to_string(),
        };
        Box::new(ClaudeHost::new(
            info,
            root.join(".claude"),
            root.join(".claude.json"),
        ))
    }

    fn opencode(root: &Path) -> Box<dyn Host> {
        let info = HostInfo {
            kind: HostKind::Opencode,
            binary: PathBuf::from("/usr/local/bin/opencode"),
            version: "2.2.6".to_string(),
        };
        Box::new(OpencodeHost::new(info, root.join(".config/opencode")))
    }

    fn engine<'a>(
        ctx: &'a Ctx,
        hosts: &'a [Box<dyn Host>],
        recipes: &'a [Recipe],
        node: &'a NodeInstall,
    ) -> Engine<'a> {
        Engine {
            ctx,
            run: &ctx.run,
            hosts,
            recipes,
            node,
        }
    }

    #[test]
    fn existing_install_is_adopted_not_reinstalled() {
        let root = tempfile::tempdir().unwrap();
        let rtk = load_all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == "rtk")
            .unwrap();
        let runner = Recorder::new(
            FakeRunner::new()
                .with_program("rtk", Path::new("/opt/homebrew/bin/rtk"))
                .reply("/opt/homebrew/bin/rtk --version", Output::ok("rtk 1.2.1\n"))
                .reply("rtk init -g", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&rtk).unwrap();

        assert_eq!(
            outcomes[0],
            (
                HostKind::Claude,
                Outcome::Adopted {
                    version: "1.2.1".to_string(),
                    route: Route::Brew
                }
            )
        );
        // Adoption probes the version, then runs only the configuration steps. No install argv runs.
        assert_eq!(
            runner.lines(),
            vec![
                "/opt/homebrew/bin/rtk --version".to_string(),
                "rtk init -g".to_string(),
                "rtk telemetry disable".to_string(),
            ]
        );
        let lock = Lockfile::load(&ctx).unwrap().unwrap();
        assert_eq!(lock.tools["rtk"].route, "brew");
    }

    /// The OpenCode fixture: EXEC_FIXTURE retargeted at OpenCode.
    fn opencode_fixture_text() -> String {
        EXEC_FIXTURE.replace("claude", "opencode")
    }

    fn opencode_fixture() -> Recipe {
        fixture(&opencode_fixture_text())
    }

    #[test]
    fn opencode_adoption_reads_the_config_not_the_path() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join(".config/opencode/opencode.jsonc");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "{\"plugins\": [\"fixture@1.2.3\"]}\n").unwrap();
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("rtk telemetry disable", Output::ok(""))
                .reply("caveman telemetry off", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&opencode_fixture()).unwrap();

        assert_eq!(
            outcomes[0],
            (
                HostKind::Opencode,
                Outcome::Adopted {
                    version: "1.2.3".to_string(),
                    route: Route::Npm
                }
            )
        );
        assert_eq!(
            runner.lines(),
            vec![
                "rtk telemetry disable".to_string(),
                "caveman telemetry off".to_string(),
            ]
        );
    }

    #[test]
    fn a_binary_on_the_path_does_not_adopt_an_opencode_install() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            FakeRunner::new()
                .with_program("fixture-pkg", Path::new("/usr/local/bin/fixture-pkg"))
                .reply("fixture-pkg --version", Output::ok("fixture-pkg 1.2.3\n"))
                .reply("fixture-pkg install", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok(""))
                .reply("caveman telemetry off", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&opencode_fixture()).unwrap();

        assert!(
            matches!(outcomes[0].1, Outcome::Installed { .. }),
            "{outcomes:?}"
        );
        assert!(
            runner.lines().contains(&"fixture-pkg install".to_string()),
            "{:?}",
            runner.lines()
        );
    }

    #[test]
    fn rtk_on_the_path_records_its_own_version_and_its_opencode_wiring_still_runs() {
        let root = tempfile::tempdir().unwrap();
        let rtk = load_all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == "rtk")
            .unwrap();
        let runner = Recorder::new(
            FakeRunner::new()
                .with_program("rtk", Path::new("/opt/homebrew/bin/rtk"))
                .reply("/opt/homebrew/bin/rtk --version", Output::ok("rtk 1.1.0\n"))
                .reply("rtk init -g --opencode", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node_install = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node_install);

        let outcomes = eng.install(&rtk).unwrap();

        // The wiring is a plugin file that detect does not see, so the install path runs its steps.
        assert!(
            outcomes.contains(&(
                HostKind::Opencode,
                Outcome::Installed {
                    version: "1.1.0".to_string()
                }
            )),
            "{outcomes:?}"
        );
        let lines = runner.lines();
        assert!(
            lines.contains(&"rtk init -g --opencode".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"rtk telemetry disable".to_string()),
            "{lines:?}"
        );
        let lock = Lockfile::load(&ctx).unwrap().unwrap();
        assert_eq!(lock.tools["rtk"].version, "1.1.0");
        assert_eq!(lock.tools["rtk"].route, "brew");
    }

    #[test]
    fn a_pinned_npm_or_binary_tool_on_opencode_locks_its_pin_when_the_config_reports_no_version() {
        for (kind, route) in [("npm", Route::Npm), ("binary", Route::Script)] {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join(".config/opencode/opencode.jsonc");
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            // The config names the tool with no version, so the wiring branch is what adopts it.
            std::fs::write(&config, "{\"plugins\": [\"fixture\"]}\n").unwrap();
            let recipe = fixture(
                &opencode_fixture_text().replace("kind = \"npm\"", &format!("kind = \"{kind}\"")),
            );
            let runner = Recorder::new(
                FakeRunner::new()
                    .reply("rtk telemetry disable", Output::ok(""))
                    .reply("caveman telemetry off", Output::ok("")),
            );
            let ctx = ctx_with(root.path(), &runner, FakeNet::new());
            let hosts = vec![opencode(root.path())];
            let node_install = node(root.path());
            let recipes = vec![];
            let eng = engine(&ctx, &hosts, &recipes, &node_install);

            let outcomes = eng.install(&recipe).unwrap();

            assert!(
                outcomes.contains(&(
                    HostKind::Opencode,
                    Outcome::Adopted {
                        version: "1.2.3".to_string(),
                        route: route.clone()
                    }
                )),
                "{kind}: {outcomes:?}"
            );
            let lock = Lockfile::load(&ctx).unwrap().unwrap();
            assert_eq!(lock.tools["fixture"].version, "1.2.3", "{kind}");
        }
    }

    #[test]
    fn a_path_binary_whose_version_cannot_be_read_is_unversioned_not_the_pin() {
        let root = tempfile::tempdir().unwrap();
        let rtk = load_all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == "rtk")
            .unwrap();
        let runner = Recorder::new(
            FakeRunner::new()
                .with_program("rtk", Path::new("/opt/homebrew/bin/rtk"))
                .reply(
                    "/opt/homebrew/bin/rtk --version",
                    Output::fail(1, "no version"),
                )
                .reply("rtk init -g --opencode", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node_install = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node_install);

        let outcomes = eng.install(&rtk).unwrap();

        assert!(
            outcomes.contains(&(
                HostKind::Opencode,
                Outcome::Installed {
                    version: UNVERSIONED.to_string()
                }
            )),
            "{outcomes:?}"
        );
        // An unreadable version is not locked, so no rtk entry is written.
        let lock = Lockfile::load(&ctx).unwrap();
        let has_rtk = lock
            .as_ref()
            .is_some_and(|lock| lock.tools.contains_key("rtk"));
        assert!(!has_rtk, "{lock:?}");
    }

    #[test]
    fn an_adopted_plugin_with_no_reported_version_is_unversioned_not_the_pin() {
        let recipe = fixture(PLUGIN_ON_BOTH_HOSTS);
        let node_root = tempfile::tempdir().unwrap();

        // Claude: the host lists the plugin without a Version field.
        let runner = Recorder::new(FakeRunner::new().reply(
            "claude plugin list",
            Output::ok(
                "❯ superpowers@anthropic-plugin-directory   Scope: user   Status: ✔ enabled\n",
            ),
        ));
        let ctx = ctx_with(node_root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(node_root.path(), "2.1.330")];
        let node_install = node(node_root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node_install);
        let outcomes = eng.install(&recipe).unwrap();
        assert_eq!(
            outcomes[0],
            (
                HostKind::Claude,
                Outcome::Adopted {
                    version: UNVERSIONED.to_string(),
                    route: Route::ClaudePlugin("anthropic-plugin-directory".to_string())
                }
            )
        );

        // OpenCode: the config lists the plugin without an @version.
        let oc_root = tempfile::tempdir().unwrap();
        let config = oc_root.path().join(".config/opencode/opencode.jsonc");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "{\"plugins\": [\"superpowers\"]}\n").unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(oc_root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(oc_root.path())];
        let node_install = node(oc_root.path());
        let eng = engine(&ctx, &hosts, &recipes, &node_install);
        let outcomes = eng.install(&recipe).unwrap();
        assert!(
            outcomes.contains(&(
                HostKind::Opencode,
                Outcome::Adopted {
                    version: UNVERSIONED.to_string(),
                    route: Route::OpencodePlugin
                }
            )),
            "{outcomes:?}"
        );
    }

    #[test]
    fn existing_plugin_keeps_its_marketplace() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(PLUGIN_FIXTURE);
        let listing = "❯ superpowers@anthropic-plugin-directory   Version: 7.0.2-22f0b463e7a5   Scope: user   Status: ✔ enabled\n";
        let runner =
            Recorder::new(FakeRunner::new().reply("claude plugin list", Output::ok(listing)));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&recipe).unwrap();

        assert_eq!(
            outcomes[0].1,
            Outcome::Adopted {
                version: "7.0.2-22f0b463e7a5".to_string(),
                route: Route::ClaudePlugin("anthropic-plugin-directory".to_string())
            }
        );
        assert_eq!(runner.lines(), vec!["claude plugin list".to_string()]);
    }

    #[test]
    fn mcp_entry_pins_the_exact_version() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(MCP_FIXTURE);
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("claude mcp remove", Output::ok(""))
                .reply("claude mcp add", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        let add: Vec<String> = runner
            .lines()
            .into_iter()
            .filter(|line| line.starts_with("claude mcp add"))
            .collect();
        assert_eq!(add.len(), 1, "{:?}", runner.lines());
        assert!(add[0].contains("@playwright/mcp@0.0.86"), "{}", add[0]);
        assert!(!add[0].contains("@latest"), "{}", add[0]);
    }

    #[test]
    fn mcp_entries_use_absolute_node_paths() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(MCP_FIXTURE);
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("claude mcp remove", Output::ok(""))
                .reply("claude mcp add", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        let npx = npx_bin(&ctx);
        let add = runner
            .cmds()
            .into_iter()
            .find(|cmd| cmd.to_string().starts_with("claude mcp add"))
            .expect("an mcp add was run");
        let argv = add.argv().to_vec();
        let separator = argv.iter().position(|a| a == "--").expect("separator");
        assert_eq!(argv[separator + 1], npx.display().to_string());
        assert!(npx.is_absolute());
        assert!(argv[separator + 1..].iter().all(|a| a != "npx"));
    }

    #[test]
    fn mcp_entries_carry_the_node_link_on_their_path() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(MCP_FIXTURE);
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("claude mcp remove", Output::ok(""))
                .reply("claude mcp add", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        let add = runner
            .cmds()
            .into_iter()
            .find(|cmd| cmd.to_string().starts_with("claude mcp add"))
            .expect("an mcp add was run");
        let argv = add.argv().to_vec();
        // Spelled out from the layout, not from the engine's helper, so a drift in either side fails the test.
        let expected = format!(
            "PATH={}:{}:/usr/bin:/bin",
            root.path().join(".local/share/relaxkit/node/bin").display(),
            root.path().join(".local/bin").display()
        );
        let flag = argv
            .iter()
            .position(|a| a == "--env")
            .expect("an --env flag");
        assert_eq!(argv[flag + 1], expected, "{argv:?}");
    }

    #[test]
    fn adopted_install_still_runs_the_opencode_sandbox_config_handler() {
        let text = r#"
id = "fixture"
title = "Fixture"
kind = "plugin"
hosts = ["opencode"]
needs_login = false
telemetry_default_off = true

[version]
npm = "fixture-pkg"
pin = "1.2.3"

[minimum_host]
opencode = "2.2.6"

[probe.opencode]
argv = ["fixture-pkg", "--help"]

[[install.opencode]]
plugin = "fixture-pkg@{version}"

[[install.opencode]]
handler = "opencode_sandbox_config"
"#;
        let recipe = fixture(text);

        let steps = config_steps(&recipe, HostKind::Opencode);

        assert!(
            steps.iter().any(
                |step| matches!(step, Step::Handler { name } if name == "opencode_sandbox_config")
            ),
            "{steps:?}"
        );
    }

    #[test]
    fn an_unsupported_handler_on_one_host_runs_nothing_on_either_host() {
        let root = tempfile::tempdir().unwrap();
        let text = r#"
id = "fixture"
title = "Fixture"
kind = "npm"
hosts = ["claude", "opencode"]
needs_login = false
telemetry_default_off = true

[version]
npm = "fixture-pkg"
pin = "1.2.3"

[minimum_host]
claude = "2.1.330"
opencode = "2.2.6"

[probe.claude]
argv = ["fixture-pkg", "--help"]

[probe.opencode]
argv = ["fixture-pkg", "--help"]

[[install.claude]]
exec = ["fixture-pkg", "install"]

[[install.opencode]]
handler = "opencode_sandbox_config"
"#;
        let mut recipe = fixture(text);
        unregistered_handler(&mut recipe);
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330"), opencode(root.path())];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let err = eng.install(&recipe).unwrap_err();

        assert!(format!("{err:#}").contains("not_yet_built"), "{err:#}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn a_recipe_that_sets_a_workdir_is_refused_before_any_step() {
        let root = tempfile::tempdir().unwrap();
        let text = EXEC_FIXTURE.replace("[version]", "workdir = \"data/serena\"\n\n[version]");
        let recipe = fixture(&text);
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let err = eng.install(&recipe).unwrap_err();

        assert!(
            format!("{err:#}")
                .contains("workdir not applied (SA-1, owner decision pending): fixture"),
            "{err:#}"
        );
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn latest_version_reads_the_registry() {
        let root = tempfile::tempdir().unwrap();
        let net = FakeNet::new()
            .with(
                "https://registry.npmjs.org/@playwright%2fmcp/latest",
                r#"{"version":"0.0.86"}"#,
            )
            .with(
                "https://api.github.com/repos/obra/superpowers/releases/latest",
                r#"{"tag_name":"v7.0.2"}"#,
            )
            .with(
                "https://pypi.org/pypi/serena-agent/json",
                r#"{"info":{"version":"2.1.0"}}"#,
            );
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, net);
        let hosts: Vec<Box<dyn Host>> = vec![];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        assert_eq!(eng.latest_version(&fixture(MCP_FIXTURE)).unwrap(), "0.0.86");
        assert_eq!(
            eng.latest_version(&fixture(PLUGIN_FIXTURE)).unwrap(),
            "7.0.2"
        );
        let serena = fixture(
            &MCP_FIXTURE
                .replace("npm = \"@playwright/mcp\"", "pypi = \"serena-agent\"")
                .replace("0.0.86", "2.1.0"),
        );
        assert_eq!(eng.latest_version(&serena).unwrap(), "2.1.0");
    }

    #[test]
    fn lockfile_records_exact_versions_after_install() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(EXEC_FIXTURE);
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("fixture-pkg install", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok(""))
                .reply("caveman telemetry off", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        let lock = Lockfile::load(&ctx).unwrap().unwrap();
        assert_eq!(
            lock.tools.get("fixture"),
            Some(&LockedTool {
                version: "1.2.3".to_string(),
                route: "npm".to_string(),
                hosts: vec!["claude".to_string()],
            })
        );
    }

    #[test]
    fn failed_step_aborts_that_tool_and_leaves_the_lock_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(EXEC_FIXTURE);
        let runner =
            Recorder::new(FakeRunner::new().reply("fixture-pkg install", Output::fail(1, "boom")));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let mut tools = BTreeMap::new();
        tools.insert(
            "other".to_string(),
            LockedTool {
                version: "9.9.9".to_string(),
                route: "npm".to_string(),
                hosts: vec!["claude".to_string()],
            },
        );
        let before = Lockfile {
            relaxkit: "0.1.0".to_string(),
            tools,
        };
        before.save(&ctx, &RunId::new()).unwrap();
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let err = eng.install(&recipe).unwrap_err();

        assert!(format!("{err:#}").contains("boom"), "{err:#}");
        assert_eq!(Lockfile::load(&ctx).unwrap(), Some(before));
        assert!(
            runner
                .lines()
                .iter()
                .all(|line| line != "rtk telemetry disable"),
            "{:?}",
            runner.lines()
        );
    }

    #[test]
    fn dry_run_runs_no_mutating_command() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(EXEC_FIXTURE);
        let runner = Recorder::new(FakeRunner::new());
        let mut ctx = ctx_with(root.path(), &runner, FakeNet::new());
        ctx.dry_run = true;
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
        assert_eq!(Lockfile::load(&ctx).unwrap(), None);
    }

    #[test]
    fn telemetry_off_steps_run_after_install() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(EXEC_FIXTURE);
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("fixture-pkg install", Output::ok(""))
                .reply("rtk telemetry disable", Output::ok(""))
                .reply("caveman telemetry off", Output::ok("")),
        );
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        eng.install(&recipe).unwrap();

        assert_eq!(
            runner.lines(),
            vec![
                "fixture-pkg install".to_string(),
                "rtk telemetry disable".to_string(),
                "caveman telemetry off".to_string(),
            ]
        );
    }

    #[test]
    fn unimplemented_handler_is_named_and_nothing_runs() {
        let root = tempfile::tempdir().unwrap();
        let text = EXEC_FIXTURE.replace(
            "[[install.claude]]\nexec = [\"rtk\", \"telemetry\", \"disable\"]",
            "[[install.claude]]\nhandler = \"serena_init\"",
        );
        let mut recipe = fixture(&text);
        unregistered_handler(&mut recipe);
        let runner = Recorder::new(FakeRunner::new().reply("fixture-pkg install", Output::ok("")));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let err = eng.install(&recipe).unwrap_err();

        assert!(format!("{err:#}").contains("not_yet_built"), "{err:#}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn rtk_claude_hook_refuses_an_opencode_host_before_any_command() {
        let root = tempfile::tempdir().unwrap();
        let text = r#"
id = "fixture"
title = "Fixture"
kind = "npm"
hosts = ["opencode"]
needs_login = false
telemetry_default_off = true

[version]
npm = "fixture-pkg"
pin = "1.2.3"

[minimum_host]
opencode = "2.2.6"

[probe.opencode]
argv = ["fixture-pkg", "--help"]

[[install.opencode]]
handler = "rtk_claude_hook"
"#;
        let recipe = fixture(text);
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let err = eng.install(&recipe).unwrap_err();

        assert!(
            format!("{err:#}").contains("only applies to Claude Code"),
            "{err:#}"
        );
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn host_below_minimum_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let text = EXEC_FIXTURE.replace("claude = \"2.1.330\"", "claude = \"2.2.0\"");
        let recipe = fixture(&text);
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![claude(root.path(), "2.1.330")];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&recipe).unwrap();

        assert!(
            matches!(outcomes[0].1, Outcome::Skipped { .. }),
            "{outcomes:?}"
        );
        assert!(runner.lines().is_empty());
    }

    #[test]
    fn host_not_detected_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let recipe = fixture(EXEC_FIXTURE);
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let hosts = vec![opencode(root.path())];
        let node = node(root.path());
        let recipes = vec![];
        let eng = engine(&ctx, &hosts, &recipes, &node);

        let outcomes = eng.install(&recipe).unwrap();

        assert_eq!(
            outcomes[0].1,
            Outcome::Skipped {
                reason: "claude is not installed".to_string()
            }
        );
    }

    #[test]
    fn a_partial_run_keeps_the_hosts_already_locked() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());
        let recipe = load_all()
            .unwrap()
            .into_iter()
            .find(|recipe| recipe.id == "rtk")
            .unwrap();
        let node = NodeInstall {
            version: "v24.13.0".to_string(),
            root: root.path().join("node"),
        };
        let engine = Engine {
            ctx: &ctx,
            run: &ctx.run,
            hosts: &[],
            recipes: &[],
            node: &node,
        };
        let lock = ctx.dirs.config.join("lock.toml");
        fs::create_dir_all(&ctx.dirs.config).unwrap();
        fs::write(
            &lock,
            "relaxkit = \"0.1.0\"\n\n[tools.rtk]\nversion = \"1.2.1\"\nroute = \"script\"\nhosts = [\"claude\", \"opencode\"]\n",
        )
        .unwrap();

        // A run that installs rtk on Claude Code only must not drop the OpenCode host from the lock (ruling K56).
        engine
            .record(
                &recipe,
                &[(HostKind::Claude, "1.2.1".to_string(), Route::Script)],
            )
            .unwrap();

        let text = fs::read_to_string(&lock).unwrap();
        let locked = Lockfile::load(&ctx).unwrap().unwrap();
        assert_eq!(
            locked.tools["rtk"].hosts,
            vec!["claude".to_string(), "opencode".to_string()],
            "{text}"
        );
    }
}
