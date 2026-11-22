//! Recipe types, the TOML shape they are read from, and the checks run when a recipe is parsed.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use serde::Deserialize;

use crate::host::HostKind;

/// Handler steps that relaxkit implements in code. A recipe may name only these.
pub const KNOWN_HANDLERS: [&str; 7] = [
    "chromium",
    "caveman_cli",
    "serena_init",
    "claude_marketplace",
    "opencode_sandbox_config",
    "rtk_claude_hook",
    "caveman_opencode",
];

/// Placeholders that `expand` substitutes into argv.
pub const PLACEHOLDERS: [&str; 5] = ["version", "node", "npx", "marketplace", "chromium"];

/// The only workdir a recipe may name: a relaxkit-owned directory under the data directory.
const KNOWN_WORKDIRS: [&str; 1] = ["data/serena"];

/// What a recipe is, for display. Nothing branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Binary,
    Npm,
    Plugin,
    Mcp,
}

/// Where relaxkit looks up a release of the tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSource {
    Github { repo: String },
    Npm { package: String },
    Pypi { package: String },
    None,
}

/// One install or update action for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Run a command. `outside_sandbox` lets it run without the sandbox on hosts that sandbox commands.
    Exec {
        argv: Vec<String>,
        outside_sandbox: bool,
    },
    /// Register an MCP server named `name` that starts with `argv`.
    Mcp { name: String, argv: Vec<String> },
    /// Install a plugin by its host-specific spec, for example `name@marketplace`.
    Plugin { spec: String },
    /// Run the code path with this name. Names are checked against `KNOWN_HANDLERS`.
    Handler { name: String },
}

/// How relaxkit checks that a tool works on one host. `expect`, when set, must appear in stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub argv: Vec<String>,
    pub expect: Option<String>,
}

/// One tool in the pack, as declared in `recipes/<id>.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub hosts: Vec<HostKind>,
    /// Admission rule (spec section 3). Always false in an accepted recipe.
    pub needs_login: bool,
    /// Declared claim that telemetry is off once relaxkit's own switches are applied.
    /// The loader cannot prove it. Always true in an accepted recipe.
    pub telemetry_default_off: bool,
    pub version: VersionSource,
    /// The exact release relaxkit installs. Set exactly when `version` names a source.
    /// `{version}` in argv and plugin specs expands to this and nothing else.
    pub pin: Option<String>,
    /// Ids of other recipes that must be installed first.
    pub requires: Vec<String>,
    /// Environment variables relaxkit sets for this tool on each host.
    pub env: BTreeMap<String, String>,
    /// Working directory for the MCP process. Only `data/serena` is accepted.
    pub workdir: Option<String>,
    pub install: BTreeMap<HostKind, Vec<Step>>,
    pub update: BTreeMap<HostKind, Vec<Step>>,
    pub probe: BTreeMap<HostKind, Probe>,
    /// Lowest host version the recipe works on, as a semver string.
    pub minimum_host: BTreeMap<HostKind, String>,
}

/// Values substituted into `{name}` placeholders in argv.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vars {
    pub node: String,
    pub npx: String,
    pub marketplace: String,
    pub chromium: String,
}

impl Vars {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "node" => Some(&self.node),
            "npx" => Some(&self.npx),
            "marketplace" => Some(&self.marketplace),
            "chromium" => Some(&self.chromium),
            _ => None,
        }
    }
}

impl Recipe {
    /// Replaces each `{name}` in every argument or plugin spec. `{version}` is this recipe's pin, and
    /// nothing else fills it. An unknown name, a missing pin, or unbalanced braces is an error.
    pub fn expand(&self, argv: &[String], vars: &Vars) -> Result<Vec<String>> {
        argv.iter()
            .map(|arg| {
                self.expand_one(arg, vars)
                    .map_err(|e| anyhow!("recipe `{}`: {e}", self.id))
            })
            .collect()
    }

    fn expand_one(&self, arg: &str, vars: &Vars) -> Result<String> {
        let mut out = String::with_capacity(arg.len());
        for piece in pieces(arg)? {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Var("version") => out.push_str(self.pin.as_deref().ok_or_else(|| {
                    anyhow!(
                        "recipe `{}` has no pin, so {{version}} cannot expand",
                        self.id
                    )
                })?),
                Piece::Var(name) => out.push_str(
                    vars.get(name)
                        .ok_or_else(|| anyhow!("unknown placeholder `{{{name}}}` in `{arg}`"))?,
                ),
            }
        }
        Ok(out)
    }
}

enum Piece<'a> {
    Text(&'a str),
    Var(&'a str),
}

fn pieces(arg: &str) -> Result<Vec<Piece<'_>>> {
    let mut out = Vec::new();
    let mut rest = arg;
    while let Some(at) = rest.find(['{', '}']) {
        if at > 0 {
            out.push(Piece::Text(&rest[..at]));
        }
        let tail = &rest[at..];
        if tail.starts_with('}') {
            bail!("stray `}}` in `{arg}`");
        }
        let close = tail
            .find('}')
            .ok_or_else(|| anyhow!("unclosed `{{` in `{arg}`"))?;
        let name = &tail[1..close];
        if name.contains('{') {
            bail!("nested `{{` in `{arg}`");
        }
        out.push(Piece::Var(name));
        rest = &tail[close + 1..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    Ok(out)
}

/// Parses and checks one recipe. The admission rule is checked first, so its error is the one a reader sees.
pub fn parse_recipe(text: &str) -> Result<Recipe> {
    let raw: RawRecipe = toml::from_str(text).context("cannot read recipe")?;
    raw.into_recipe()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    id: String,
    title: String,
    kind: Kind,
    hosts: Vec<String>,
    needs_login: bool,
    telemetry_default_off: bool,
    #[serde(default)]
    version: Option<RawVersion>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    install: BTreeMap<String, Vec<RawStep>>,
    #[serde(default)]
    update: BTreeMap<String, Vec<RawStep>>,
    #[serde(default)]
    probe: BTreeMap<String, RawProbe>,
    #[serde(default)]
    minimum_host: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVersion {
    github: Option<String>,
    npm: Option<String>,
    pypi: Option<String>,
    pin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    exec: Option<Vec<String>>,
    outside_sandbox: Option<bool>,
    mcp: Option<String>,
    argv: Option<Vec<String>>,
    plugin: Option<String>,
    handler: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProbe {
    argv: Vec<String>,
    expect: Option<String>,
}

impl RawRecipe {
    fn into_recipe(self) -> Result<Recipe> {
        let id = self.id;
        if self.needs_login {
            bail!(
                "recipe `{id}` breaks the admission rule: needs_login must be false (spec section 3)"
            );
        }
        if !self.telemetry_default_off {
            bail!(
                "recipe `{id}` breaks the admission rule: telemetry_default_off must be true (spec section 3)"
            );
        }
        if id.trim().is_empty() || self.title.trim().is_empty() {
            bail!("recipe `{id}` needs a non-empty id and title");
        }
        let hosts = parse_hosts(&id, &self.hosts)?;
        let (version, pin) = match self.version {
            Some(raw) => raw.into_parts(&id)?,
            None => (VersionSource::None, None),
        };
        if let Some(workdir) = self
            .workdir
            .as_ref()
            .filter(|dir| !KNOWN_WORKDIRS.contains(&dir.as_str()))
        {
            bail!(
                "recipe `{id}` names workdir `{workdir}`, which is not a relaxkit data directory"
            );
        }
        let install = convert_steps(&id, &hosts, self.install)?;
        let update = convert_steps(&id, &hosts, self.update)?;
        let mut probe = BTreeMap::new();
        for (name, raw) in self.probe {
            let host = host_key(&id, &name, &hosts)?;
            if raw.argv.is_empty() {
                bail!("recipe `{id}` has an empty probe argv for {host}");
            }
            probe.insert(
                host,
                Probe {
                    argv: raw.argv,
                    expect: raw.expect,
                },
            );
        }
        let mut minimum_host = BTreeMap::new();
        for (name, value) in self.minimum_host {
            let host = host_key(&id, &name, &hosts)?;
            Version::parse(&value).with_context(|| {
                format!("recipe `{id}` gives minimum_host {host} = `{value}`, which is not semver")
            })?;
            minimum_host.insert(host, value);
        }
        for host in &hosts {
            if !probe.contains_key(host) {
                bail!("recipe `{id}` supports {host} but has no probe for it");
            }
        }
        for host in &hosts {
            if !minimum_host.contains_key(host) {
                bail!("recipe `{id}` supports {host} but has no minimum_host floor for it");
            }
        }
        let recipe = Recipe {
            id,
            title: self.title,
            kind: self.kind,
            hosts,
            needs_login: self.needs_login,
            telemetry_default_off: self.telemetry_default_off,
            version,
            pin,
            requires: self.requires,
            env: self.env,
            workdir: self.workdir,
            install,
            update,
            probe,
            minimum_host,
        };
        check_placeholders(&recipe)?;
        Ok(recipe)
    }
}

impl RawVersion {
    /// A source and its pin come together: a source needs a semver pin, and a pin needs a source.
    fn into_parts(self, id: &str) -> Result<(VersionSource, Option<String>)> {
        let mut found = Vec::new();
        if let Some(repo) = self.github {
            found.push(VersionSource::Github { repo });
        }
        if let Some(package) = self.npm {
            found.push(VersionSource::Npm { package });
        }
        if let Some(package) = self.pypi {
            found.push(VersionSource::Pypi { package });
        }
        let source = match found.len() {
            0 => {
                if self.pin.is_some() {
                    bail!("recipe `{id}` names a pin but no version source");
                }
                return Ok((VersionSource::None, None));
            }
            1 => found.remove(0),
            _ => bail!("recipe `{id}` names more than one version source"),
        };
        let empty = match &source {
            VersionSource::Github { repo } => repo.trim().is_empty(),
            VersionSource::Npm { package } | VersionSource::Pypi { package } => {
                package.trim().is_empty()
            }
            VersionSource::None => false,
        };
        if empty {
            bail!("recipe `{id}` has an empty version source");
        }
        let pin = self
            .pin
            .ok_or_else(|| anyhow!("recipe `{id}` names a version source but no pin"))?;
        Version::parse(&pin)
            .with_context(|| format!("recipe `{id}` pins `{pin}`, which is not semver"))?;
        Ok((source, Some(pin)))
    }
}

impl RawStep {
    fn into_step(self, id: &str) -> Result<Step> {
        let kinds = [
            self.exec.is_some(),
            self.mcp.is_some(),
            self.plugin.is_some(),
            self.handler.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count();
        if kinds != 1 {
            bail!("recipe `{id}`: each step names exactly one of exec, mcp, plugin or handler");
        }
        if self.argv.is_some() && self.mcp.is_none() {
            bail!("recipe `{id}`: argv is only valid on an mcp step");
        }
        if self.outside_sandbox.is_some() && self.exec.is_none() {
            bail!("recipe `{id}`: outside_sandbox is only valid on an exec step");
        }
        if let Some(argv) = self.exec {
            non_empty_argv(id, &argv)?;
            return Ok(Step::Exec {
                argv,
                outside_sandbox: self.outside_sandbox.unwrap_or(false),
            });
        }
        if let Some(name) = self.mcp {
            let argv = self
                .argv
                .ok_or_else(|| anyhow!("recipe `{id}`: mcp step `{name}` needs argv"))?;
            non_empty_argv(id, &argv)?;
            return Ok(Step::Mcp { name, argv });
        }
        if let Some(spec) = self.plugin {
            if spec.trim().is_empty() {
                bail!("recipe `{id}`: plugin step has an empty spec");
            }
            return Ok(Step::Plugin { spec });
        }
        let name = self
            .handler
            .ok_or_else(|| anyhow!("recipe `{id}`: step has no kind"))?;
        if !KNOWN_HANDLERS.contains(&name.as_str()) {
            bail!("recipe `{id}` names unknown handler `{name}`");
        }
        Ok(Step::Handler { name })
    }
}

fn non_empty_argv(id: &str, argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv[0].trim().is_empty() {
        bail!("recipe `{id}` has an empty command");
    }
    Ok(())
}

fn parse_hosts(id: &str, names: &[String]) -> Result<Vec<HostKind>> {
    if names.is_empty() {
        bail!("recipe `{id}` lists no hosts");
    }
    let mut hosts = Vec::new();
    for name in names {
        let host: HostKind = name.parse().with_context(|| format!("recipe `{id}`"))?;
        if hosts.contains(&host) {
            bail!("recipe `{id}` lists {host} twice");
        }
        hosts.push(host);
    }
    Ok(hosts)
}

fn host_key(id: &str, name: &str, hosts: &[HostKind]) -> Result<HostKind> {
    let host: HostKind = name.parse().with_context(|| format!("recipe `{id}`"))?;
    if !hosts.contains(&host) {
        bail!("recipe `{id}` has data for {host}, which is not one of its hosts");
    }
    Ok(host)
}

fn convert_steps(
    id: &str,
    hosts: &[HostKind],
    raw: BTreeMap<String, Vec<RawStep>>,
) -> Result<BTreeMap<HostKind, Vec<Step>>> {
    let mut out = BTreeMap::new();
    for (name, steps) in raw {
        let host = host_key(id, &name, hosts)?;
        let converted = steps
            .into_iter()
            .map(|step| step.into_step(id))
            .collect::<Result<Vec<_>>>()?;
        out.insert(host, converted);
    }
    Ok(out)
}

/// Every placeholder must be known, and `{version}` needs a pin to fill it. Covers argv, probes and plugin specs.
fn check_placeholders(recipe: &Recipe) -> Result<()> {
    let mut texts: Vec<&String> = Vec::new();
    for step in recipe
        .install
        .values()
        .chain(recipe.update.values())
        .flatten()
    {
        match step {
            Step::Exec { argv, .. } | Step::Mcp { argv, .. } => texts.extend(argv),
            Step::Plugin { spec } => texts.push(spec),
            Step::Handler { .. } => {}
        }
    }
    texts.extend(recipe.probe.values().flat_map(|probe| &probe.argv));
    for text in texts {
        let parsed = pieces(text).map_err(|e| anyhow!("recipe `{}`: {e}", recipe.id))?;
        for piece in parsed {
            if let Piece::Var(name) = piece {
                if !PLACEHOLDERS.contains(&name) {
                    bail!(
                        "recipe `{}` uses unknown placeholder `{{{name}}}`",
                        recipe.id
                    );
                }
                if name == "version" && recipe.pin.is_none() {
                    bail!("recipe `{}` uses {{version}} but has no pin", recipe.id);
                }
            }
        }
    }
    Ok(())
}
