//! `relaxkit config set|unset|show` (spec 6.1, 6.2). A role setting, the voice and the compact threshold are saved in
//! `config.toml` and written to the hosts' own files through the `config_apply` editors. `--host` limits a role to one
//! host (saved under `[hosts.<host>.roles]`). `--project` writes the project-level files instead and leaves
//! `config.toml` alone. Every write goes through `state::safe_write`, so `relaxkit undo` reverts it.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{KitConfig, effective, parse_compact};
use crate::config_apply::{claude, opencode, voice};
use crate::ctx::Ctx;
use crate::edit::jsonc_remove;
use crate::exec::Cmd;
use crate::host::{Host, HostInfo, HostKind, InstalledPlugin, detect_hosts};
use crate::presets::Voice;
use crate::roles::{ModelSpec, Role, RoleSetting, resolve_claude, resolve_opencode};
use crate::state::{RunId, with_run_lock};

/// The two hosts, in the order their settings are written.
const HOSTS: [HostKind; 2] = [HostKind::Claude, HostKind::Opencode];

/// What a config key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Role(Role),
    Voice,
    CompactAt,
}

fn parse_key(key: &str) -> Result<Key> {
    Ok(match key {
        "planner" => Key::Role(Role::Planner),
        "implementer" => Key::Role(Role::Implementer),
        "reviewer" => Key::Role(Role::Reviewer),
        "escalation" => Key::Role(Role::Escalation),
        "final-review" => Key::Role(Role::FinalReview),
        "voice" => Key::Voice,
        "compact-at" => Key::CompactAt,
        other => {
            bail!("unknown key `{other}`; use planner, implementer, reviewer, voice or compact-at")
        }
    })
}

/// Parses a role value. `best` is not a tier in this product, so it gets a hint instead of the generic error.
fn parse_setting(value: &str) -> Result<RoleSetting> {
    if value.trim() == "best" {
        bail!(
            "`best` is not a tier; use cheap, standard or smarter (smarter is the strongest), or an exact model such as anthropic/claude-opus-5-5"
        );
    }
    RoleSetting::parse(value)
}

/// `relaxkit config set <key> <value> [--host]`.
pub fn set(ctx: &Ctx, key: &str, value: &str, host: Option<HostKind>) -> Result<()> {
    match parse_key(key)? {
        Key::Role(role) => {
            let setting = parse_setting(value)?;
            locked(ctx, |run| set_role(ctx, run, role, value, setting, host))
        }
        Key::Voice => {
            no_host_for(host, key)?;
            let voice = Voice::parse(value)?;
            locked(ctx, |run| set_voice(ctx, run, voice))
        }
        Key::CompactAt => {
            no_host_for(host, key)?;
            let tokens = parse_compact(value)?;
            locked(ctx, |run| set_compact(ctx, run, tokens))
        }
    }
}

/// `relaxkit config unset <key> [--host]`: the role goes back to its preset tier, the voice to the preset's voice.
pub fn unset(ctx: &Ctx, key: &str, host: Option<HostKind>) -> Result<()> {
    match parse_key(key)? {
        Key::Role(role) => locked(ctx, |run| unset_role(ctx, run, role, host)),
        Key::Voice => {
            no_host_for(host, key)?;
            locked(ctx, |run| {
                let mut config = KitConfig::load(&ctx.dirs)?;
                config.voice = None;
                save_global(ctx, run, &config)?;
                voice::apply(ctx, run, config.preset.voice(), ctx.project.is_some())?;
                ctx.out.info(&format!(
                    "voice back to the {} preset's {}",
                    config.preset.as_str(),
                    config.preset.voice().as_str()
                ));
                Ok(())
            })
        }
        Key::CompactAt => {
            no_host_for(host, key)?;
            locked(ctx, |run| unset_compact(ctx, run))
        }
    }
}

/// The lines of `relaxkit config show [<key>]`: the preset, the voice and the threshold, then one row per role. A
/// role row names what each host runs with.
pub fn show(ctx: &Ctx, key: Option<&str>) -> Result<Vec<String>> {
    let view = View::load(ctx)?;
    let mut lines = Vec::new();
    match key.map(parse_key).transpose()? {
        None => {
            lines.push(format!("preset: {}", view.config.preset.as_str()));
            lines.push(format!("voice: {}", view.voice_label()));
            lines.push(format!("compact-at: {}", view.compact_label()));
            for row in view.rows() {
                lines.push(row.text());
            }
        }
        Some(Key::Role(role)) => lines.push(view.row(role).text()),
        Some(Key::Voice) => lines.push(format!("voice: {}", view.voice_label())),
        Some(Key::CompactAt) => lines.push(format!("compact-at: {}", view.compact_label())),
    }
    if !view.opencode_listed() && view.has_host(HostKind::Opencode) {
        lines.push(
            "note: OpenCode's model list could not be read, so OpenCode models show as inherited"
                .to_string(),
        );
    }
    Ok(lines)
}

/// The JSON document of `relaxkit config show --json` (the spec 6.1 table): both hosts for every role, with the
/// preset, the voice and the threshold. With a key, only that entry.
pub fn show_json(ctx: &Ctx, key: Option<&str>) -> Result<Value> {
    let view = View::load(ctx)?;
    let mut document = json!({
        "preset": view.config.preset.as_str(),
        "voice": view.voice_label_word(),
        "compact_at": view.config.compact_at,
    });
    match key.map(parse_key).transpose()? {
        None => {
            document["roles"] = Value::Array(view.rows().iter().map(RoleRow::json).collect());
        }
        Some(Key::Role(role)) => {
            document["roles"] = Value::Array(vec![view.row(role).json()]);
        }
        Some(Key::Voice) => {
            document = json!({"voice": view.voice_label_word()});
        }
        Some(Key::CompactAt) => {
            document = json!({"compact_at": view.config.compact_at});
        }
    }
    Ok(document)
}

// ---- writes ----------------------------------------------------------------------------------------------------

/// Runs `body` under the run lock, as every config write does. A dry run takes no lock and writes nothing.
fn locked<T>(ctx: &Ctx, body: impl FnOnce(&RunId) -> Result<T>) -> Result<T> {
    if ctx.dry_run {
        return body(&ctx.run);
    }
    with_run_lock(&ctx.dirs, || body(&ctx.run))
}

fn no_host_for(host: Option<HostKind>, key: &str) -> Result<()> {
    match host {
        None => Ok(()),
        Some(_) => bail!("--host applies to roles only; `{key}` applies to both hosts"),
    }
}

/// Saves `config.toml`, unless the change is project-level (`--project`), which leaves the global file alone.
fn save_global(ctx: &Ctx, run: &RunId, config: &KitConfig) -> Result<()> {
    if ctx.project.is_none() {
        config.save(ctx, run)?;
    }
    Ok(())
}

/// K123: a tier on OpenCode needs the model list. When the list cannot be read, a change that leaves one of `roles` as
/// a tier on OpenCode is refused before any write. Pins need no list, so they are not refused.
pub(crate) fn refuse_tier_without_list(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    targets: &[HostKind],
    config: &KitConfig,
    roles: &[Role],
) -> Result<()> {
    if !targets.contains(&HostKind::Opencode) {
        return Ok(());
    }
    let Some(host) = hosts
        .iter()
        .find(|host| host.info().kind == HostKind::Opencode)
    else {
        return Ok(());
    };
    let own = config.for_host(HostKind::Opencode);
    let tier_left = roles
        .iter()
        .any(|role| matches!(effective(&own, *role), RoleSetting::Tier(_)));
    if !tier_left || listing(ctx, host.as_ref()).is_some() {
        return Ok(());
    }
    bail!(
        "OpenCode's model list could not be read, so a tier cannot be applied to OpenCode now: pin an exact model (provider/model) or retry when `opencode models` works"
    )
}

fn set_role(
    ctx: &Ctx,
    run: &RunId,
    role: Role,
    value: &str,
    setting: RoleSetting,
    host: Option<HostKind>,
) -> Result<()> {
    let hosts = scoped_hosts(ctx)?;
    let targets: Vec<HostKind> = host.map_or_else(|| HOSTS.to_vec(), |kind| vec![kind]);
    if let RoleSetting::Pin(spec) = &setting {
        check_pin(ctx, &hosts, &targets, spec, value)?;
    }
    let mut config = KitConfig::load(&ctx.dirs)?;
    put_role(&mut config, role, setting, host);
    refuse_tier_without_list(ctx, &hosts, &targets, &config, &[role])?;
    save_global(ctx, run, &config)?;
    apply_roles(ctx, run, &hosts, &config, host)?;
    let row = View::build(ctx, config).row(role);
    for kind in targets {
        ctx.out.info(&format!(
            "{} on {}: {}",
            role.key(),
            kind.product_name(),
            row.host(kind).note
        ));
    }
    ctx.out.info("takes effect in new sessions");
    Ok(())
}

fn unset_role(ctx: &Ctx, run: &RunId, role: Role, host: Option<HostKind>) -> Result<()> {
    let hosts = scoped_hosts(ctx)?;
    let targets: Vec<HostKind> = host.map_or_else(|| HOSTS.to_vec(), |kind| vec![kind]);
    let mut config = KitConfig::load(&ctx.dirs)?;
    remove_role(&mut config, role, host);
    refuse_tier_without_list(ctx, &hosts, &targets, &config, &[role])?;
    save_global(ctx, run, &config)?;
    apply_roles(ctx, run, &hosts, &config, host)?;
    let view = View::build(ctx, config);
    ctx.out.info(&format!(
        "{} back to its preset tier: {}",
        role.key(),
        view.row(role).setting
    ));
    Ok(())
}

fn set_voice(ctx: &Ctx, run: &RunId, voice_word: Voice) -> Result<()> {
    let mut config = KitConfig::load(&ctx.dirs)?;
    config.voice = Some(voice_word);
    save_global(ctx, run, &config)?;
    voice::apply(ctx, run, voice_word, ctx.project.is_some())?;
    ctx.out.info(&format!("voice: {}", voice_word.as_str()));
    Ok(())
}

fn set_compact(ctx: &Ctx, run: &RunId, tokens: u64) -> Result<()> {
    let hosts = scoped_hosts(ctx)?;
    let mut config = KitConfig::load(&ctx.dirs)?;
    config.compact_at = Some(tokens);
    save_global(ctx, run, &config)?;
    for host in &hosts {
        match host.info().kind {
            HostKind::Claude => {
                claude::apply_compact(
                    ctx,
                    run,
                    host.as_ref(),
                    &config.for_host(HostKind::Claude),
                    tokens,
                )?;
            }
            HostKind::Opencode => {
                // K114: no verified window source exists for OpenCode yet, so no buffer is written.
                match opencode::apply_compact(ctx, run, host.as_ref(), tokens, None)? {
                    opencode::CompactNote::Applied { .. } => {}
                    opencode::CompactNote::KeptDefault => ctx.out.warn(
                        "OpenCode's compaction buffer is not written: there is no verified window source for its default model yet, so OpenCode keeps its own threshold",
                    ),
                }
            }
        }
    }
    ctx.out.info(&format!("compact-at: {tokens} tokens"));
    Ok(())
}

fn unset_compact(ctx: &Ctx, run: &RunId) -> Result<()> {
    let hosts = scoped_hosts(ctx)?;
    let mut config = KitConfig::load(&ctx.dirs)?;
    for host in &hosts {
        match host.info().kind {
            HostKind::Claude => {
                let models = claude::claude_models(&config.for_host(HostKind::Claude));
                let path = host.config_file(ctx);
                let old = read_or_empty(&path)?;
                let mut text = old.clone();
                for model in &models {
                    text = jsonc_remove(&text, &["modelSettings", model, "autoCompactWindow"])?;
                }
                write_changed(ctx, run, &path, &old, &text)?;
            }
            HostKind::Opencode => {
                let path = host.config_file(ctx);
                let old = read_or_empty(&path)?;
                let text = jsonc_remove(&old, &["compaction", "buffer"])?;
                write_changed(ctx, run, &path, &old, &text)?;
            }
        }
    }
    config.compact_at = None;
    save_global(ctx, run, &config)?;
    ctx.out.info("compact-at: not set");
    Ok(())
}

/// Writes the roles of `config` to each host (only `only` when given). A host that is not installed is skipped.
pub(crate) fn apply_roles(
    ctx: &Ctx,
    run: &RunId,
    hosts: &[Box<dyn Host>],
    config: &KitConfig,
    only: Option<HostKind>,
) -> Result<()> {
    for host in hosts {
        let kind = host.info().kind;
        if only.is_some_and(|wanted| wanted != kind) {
            continue;
        }
        match kind {
            HostKind::Claude => {
                claude::apply_roles(ctx, run, host.as_ref(), &config.for_host(HostKind::Claude))?;
            }
            HostKind::Opencode => {
                let ids = listing(ctx, host.as_ref()).unwrap_or_default();
                opencode::apply_roles(
                    ctx,
                    run,
                    host.as_ref(),
                    &config.for_host(HostKind::Opencode),
                    &ids,
                )?;
            }
        }
    }
    Ok(())
}

/// Sets a role for one host (`Some`) or for both (`None`). A change for both clears any host-only value, so the
/// two hosts end up on the same setting.
fn put_role(config: &mut KitConfig, role: Role, setting: RoleSetting, host: Option<HostKind>) {
    match host {
        None => {
            config.roles.insert(role, setting);
            for kind in HOSTS {
                remove_host_role(config, kind, role);
            }
        }
        Some(kind) => {
            config
                .host_roles
                .entry(kind)
                .or_default()
                .insert(role, setting);
        }
    }
}

/// Removes a role's setting: the shared one and any host-only one (`None`), or only the one for `host`.
fn remove_role(config: &mut KitConfig, role: Role, host: Option<HostKind>) {
    match host {
        None => {
            config.roles.remove(&role);
            for kind in HOSTS {
                remove_host_role(config, kind, role);
            }
        }
        Some(kind) => remove_host_role(config, kind, role),
    }
}

fn remove_host_role(config: &mut KitConfig, host: HostKind, role: Role) {
    if let Some(roles) = config.host_roles.get_mut(&host) {
        roles.remove(&role);
        if roles.is_empty() {
            config.host_roles.remove(&host);
        }
    }
}

// ---- validation ------------------------------------------------------------------------------------------------

/// Checks an exact pin (spec 6.1, rule 4). OpenCode's model must be in `opencode models`; an unknown name is rejected
/// with the closest matches, and an unreadable list accepts the pin with a warning. Claude Code takes aliases and
/// `claude-*` ids, and warns on anything else. Without an OpenCode host there is no list, so nothing is checked there.
fn check_pin(
    ctx: &Ctx,
    hosts: &[Box<dyn Host>],
    targets: &[HostKind],
    spec: &ModelSpec,
    value: &str,
) -> Result<()> {
    if targets.contains(&HostKind::Opencode) {
        match hosts
            .iter()
            .find(|host| host.info().kind == HostKind::Opencode)
        {
            // K109 / F23-4: the pin is still stored, since OpenCode may be installed later.
            None => ctx.out.warn(&format!(
                "OpenCode is not installed, so `{value}` is not checked against its model list"
            )),
            Some(host) => {
                let id = format!("{}/{}", spec.provider, spec.model);
                match listing(ctx, host.as_ref()) {
                    None => ctx.out.warn(&format!(
                        "could not read `opencode models`, so `{value}` is accepted without a check"
                    )),
                    Some(ids) if ids.contains(&id) => {}
                    Some(ids) => bail!(
                        "`{value}` is not a model OpenCode lists. Closest matches: {}. `opencode models` prints the full list",
                        closest(&id, &ids).join(", ")
                    ),
                }
            }
        }
    }
    if targets.contains(&HostKind::Claude) && spec.provider == "anthropic" {
        let known = ["haiku", "sonnet", "opus", "fable"].contains(&spec.model.as_str())
            || spec.model.starts_with("claude-");
        if !known {
            ctx.out.warn(&format!(
                "Claude Code takes aliases and claude-* ids; `{value}` may not work there"
            ));
        }
    }
    Ok(())
}

/// Up to three names closest to `wanted` by edit distance, nearest first, ties by name.
pub fn closest(wanted: &str, names: &[String]) -> Vec<String> {
    let mut ranked: Vec<(usize, &String)> = names
        .iter()
        .map(|name| (edit_distance(wanted, name), name))
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .take(3)
        .map(|(_, name)| name.clone())
        .collect()
}

/// The Levenshtein distance between two strings, over characters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = diagonal + usize::from(ca != *cb);
            diagonal = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

// ---- host access -----------------------------------------------------------------------------------------------

/// The OpenCode model ids from `opencode models`, or `None` when the list cannot be read or is empty.
fn listing(ctx: &Ctx, host: &dyn Host) -> Option<Vec<String>> {
    let binary = host.info().binary.to_string_lossy().into_owned();
    let out = ctx.runner.run(&Cmd::new(&binary).arg("models")).ok()?;
    if out.code != 0 {
        return None;
    }
    let ids: Vec<String> = out
        .stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// The hosts this command acts on. With `--project` they are wrapped so their files are the project's.
fn scoped_hosts(ctx: &Ctx) -> Result<Vec<Box<dyn Host>>> {
    let hosts = detect_hosts(ctx);
    let Some(project) = ctx.project.clone() else {
        return Ok(hosts);
    };
    Ok(hosts
        .into_iter()
        .map(|host| -> Box<dyn Host> {
            let (instructions, config) = project_files(&project, host.info().kind);
            Box::new(ProjectScope {
                inner: host,
                instructions,
                config,
            })
        })
        .collect())
}

/// The project-level instruction file and config file of a host (`--project`).
fn project_files(project: &std::path::Path, kind: HostKind) -> (PathBuf, PathBuf) {
    match kind {
        HostKind::Claude => (
            project.join("CLAUDE.md"),
            project.join(".claude").join("settings.json"),
        ),
        HostKind::Opencode => (
            project.join("AGENTS.md"),
            project.join(".opencode").join("opencode.jsonc"),
        ),
    }
}

fn read_or_empty(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_changed(
    ctx: &Ctx,
    run: &RunId,
    path: &std::path::Path,
    old: &str,
    new: &str,
) -> Result<()> {
    if old == new {
        return Ok(());
    }
    crate::state::safe_write(ctx, run, path, new)
}

/// A host whose instruction and config files are the project's (`--project`). Everything else is the host's own.
struct ProjectScope {
    inner: Box<dyn Host>,
    instructions: PathBuf,
    config: PathBuf,
}

impl Host for ProjectScope {
    fn info(&self) -> &HostInfo {
        self.inner.info()
    }

    fn instructions_file(&self, _ctx: &Ctx) -> PathBuf {
        self.instructions.clone()
    }

    fn config_file(&self, _ctx: &Ctx) -> PathBuf {
        self.config.clone()
    }

    fn mcp_add(
        &self,
        ctx: &Ctx,
        name: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<()> {
        self.inner.mcp_add(ctx, name, argv, env)
    }

    fn mcp_remove(&self, ctx: &Ctx, name: &str) -> Result<()> {
        self.inner.mcp_remove(ctx, name)
    }

    fn mcp_entry_present(&self, ctx: &Ctx, name: &str) -> Result<bool> {
        self.inner.mcp_entry_present(ctx, name)
    }

    fn plugin_install(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        self.inner.plugin_install(ctx, spec)
    }

    fn plugin_update(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        self.inner.plugin_update(ctx, spec)
    }

    fn plugin_list(&self, ctx: &Ctx) -> Result<Vec<InstalledPlugin>> {
        self.inner.plugin_list(ctx)
    }

    fn marketplace_add(&self, ctx: &Ctx, source: &str) -> Result<()> {
        self.inner.marketplace_add(ctx, source)
    }

    fn marketplace_update(&self, ctx: &Ctx) -> Result<()> {
        self.inner.marketplace_update(ctx)
    }

    fn state_files(&self, ctx: &Ctx) -> Vec<PathBuf> {
        self.inner.state_files(ctx)
    }
}

// ---- the view a command reads --------------------------------------------------------------------------------

/// The config, the hosts that are installed, and the OpenCode model list, as `show` and the set report read them.
struct View {
    config: KitConfig,
    claude_present: bool,
    opencode_present: bool,
    /// `None` when the OpenCode list cannot be read.
    models: Option<Vec<String>>,
}

impl View {
    fn load(ctx: &Ctx) -> Result<View> {
        let config = KitConfig::load(&ctx.dirs)?;
        Ok(View::build(ctx, config))
    }

    fn build(ctx: &Ctx, config: KitConfig) -> View {
        let hosts = detect_hosts(ctx);
        let opencode = hosts
            .iter()
            .find(|host| host.info().kind == HostKind::Opencode);
        View {
            config,
            claude_present: hosts
                .iter()
                .any(|host| host.info().kind == HostKind::Claude),
            opencode_present: opencode.is_some(),
            models: opencode.and_then(|host| listing(ctx, host.as_ref())),
        }
    }

    fn has_host(&self, kind: HostKind) -> bool {
        match kind {
            HostKind::Claude => self.claude_present,
            HostKind::Opencode => self.opencode_present,
        }
    }

    fn opencode_listed(&self) -> bool {
        self.models.is_some()
    }

    fn voice_word(&self) -> Voice {
        self.config
            .voice
            .unwrap_or_else(|| self.config.preset.voice())
    }

    fn voice_label(&self) -> String {
        match self.config.voice {
            Some(voice_word) => voice_word.as_str().to_string(),
            None => format!("{} (preset)", self.voice_word().as_str()),
        }
    }

    fn voice_label_word(&self) -> &'static str {
        self.voice_word().as_str()
    }

    fn compact_label(&self) -> String {
        match self.config.compact_at {
            Some(tokens) => format!("{tokens} tokens"),
            None => "not set".to_string(),
        }
    }

    fn rows(&self) -> Vec<RoleRow> {
        Role::ALL.into_iter().map(|role| self.row(role)).collect()
    }

    fn row(&self, role: Role) -> RoleRow {
        let shared = effective(&self.config, role);
        let pinned = self
            .config
            .roles
            .get(&role)
            .is_some_and(|s| matches!(s, RoleSetting::Pin(_)))
            || self.config.host_roles.values().any(|roles| {
                roles
                    .get(&role)
                    .is_some_and(|s| matches!(s, RoleSetting::Pin(_)))
            });
        RoleRow {
            role,
            setting: shared.to_config_string(),
            pinned,
            claude: self.claude_cell(role),
            opencode: self.opencode_cell(role),
        }
    }

    /// Claude Code: an alias for the setting. A pin to another provider has no alias, so the tier it leaves is shown.
    fn claude_cell(&self, role: Role) -> HostCell {
        let own = self.config.for_host(HostKind::Claude);
        let setting = effective(&own, role).to_config_string();
        let other_provider = matches!(
            own.roles.get(&role),
            Some(RoleSetting::Pin(spec)) if spec.provider != "anthropic"
        );
        let mut kept = own.clone();
        kept.roles
            .retain(|_, s| !matches!(s, RoleSetting::Pin(spec) if spec.provider != "anthropic"));
        let model = resolve_claude(&effective(&kept, role));
        let note = match (&model, other_provider) {
            (Some(alias), true) => format!(
                "{alias} (advisory; the tier is kept, since a non-Anthropic pin applies to OpenCode only)"
            ),
            (Some(alias), false) => format!("{alias} (advisory)"),
            (None, _) => "inherits the host default".to_string(),
        };
        let note = if self.claude_present {
            note
        } else {
            "(Claude Code not found)".to_string()
        };
        HostCell {
            setting,
            model,
            note,
        }
    }

    fn opencode_cell(&self, role: Role) -> HostCell {
        let own = self.config.for_host(HostKind::Opencode);
        let setting = effective(&own, role);
        let setting_text = setting.to_config_string();
        if !self.opencode_present {
            return HostCell {
                setting: setting_text,
                model: None,
                note: "(OpenCode not found)".to_string(),
            };
        }
        let mode = if role == Role::Planner {
            "advisory"
        } else {
            "enforced per agent"
        };
        // A pin names its model without the list; a tier needs the list, so without it the note says so.
        let models = self.models.clone().unwrap_or_default();
        if self.models.is_none() && !matches!(setting, RoleSetting::Pin(_)) {
            return HostCell {
                setting: setting_text,
                model: None,
                note: "inherits the host default (the model list cannot be read)".to_string(),
            };
        }
        match resolve_opencode(&setting, &models) {
            Some(model) => HostCell {
                setting: setting_text,
                model: Some(model.clone()),
                note: format!("{model} ({mode})"),
            },
            None => HostCell {
                setting: setting_text,
                model: None,
                note: "inherits the host default".to_string(),
            },
        }
    }
}

/// One host's column of a role row.
struct HostCell {
    /// The setting the host runs with, after its own overrides.
    setting: String,
    /// The model the host is given, when relaxkit can name one.
    model: Option<String>,
    /// The words for the host, as `show` and the set report print them.
    note: String,
}

/// One role across both hosts.
struct RoleRow {
    role: Role,
    setting: String,
    pinned: bool,
    claude: HostCell,
    opencode: HostCell,
}

impl RoleRow {
    fn host(&self, kind: HostKind) -> &HostCell {
        match kind {
            HostKind::Claude => &self.claude,
            HostKind::Opencode => &self.opencode,
        }
    }

    fn text(&self) -> String {
        let pin = if self.pinned { " (pinned)" } else { "" };
        format!(
            "{}  {}{pin}\n  claude-code: {}\n  opencode: {}",
            self.role.key(),
            self.setting,
            self.claude.note,
            self.opencode.note
        )
    }

    fn json(&self) -> Value {
        json!({
            "role": self.role.key(),
            "setting": self.setting,
            "pinned": self.pinned,
            "claude-code": {"setting": self.claude.setting, "model": self.claude.model, "note": self.claude.note},
            "opencode": {"setting": self.opencode.setting, "model": self.opencode.model, "note": self.opencode.note},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_names_are_ranked_by_edit_distance() {
        let names: Vec<String> = [
            "anthropic/claude-haiku-5-5",
            "anthropic/claude-opus-5-5",
            "openai/gpt-x",
            "anthropic/claude-sonnet-5-5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let found = closest("anthropic/claude-hiku-5-5", &names);
        assert_eq!(found[0], "anthropic/claude-haiku-5-5");
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn edit_distance_counts_insertions_deletions_and_substitutions() {
        assert_eq!(edit_distance("haiku", "haiku"), 0);
        assert_eq!(edit_distance("hiku", "haiku"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn best_gets_the_smarter_hint() {
        let err = format!("{:#}", parse_setting("best").unwrap_err());
        assert!(err.contains("smarter"), "{err}");
    }
}
