//! The kit's own `config.toml`: preset, per-role settings, voice and compaction threshold.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{DocumentMut, Item};

use crate::ctx::Ctx;
use crate::edit::{toml_get, toml_set};
use crate::platform::KitDirs;
use crate::presets::{Preset, Voice};
use crate::roles::{Role, RoleSetting, tier_of};
use crate::state::{RunId, safe_write};

const CONFIG_FILE: &str = "config.toml";

/// The parsed `config.toml`. A missing file reads as `KitConfig::default()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KitConfig {
    pub preset: Preset,
    pub roles: BTreeMap<Role, RoleSetting>,
    /// `None` means the preset's own voice.
    pub voice: Option<Voice>,
    pub compact_at: Option<u64>,
}

impl KitConfig {
    /// Reads `dirs.config/config.toml`. A missing file gives the default config.
    pub fn load(dirs: &KitDirs) -> Result<KitConfig> {
        let path = config_path(dirs);
        let Some(text) = read_text(&path)? else {
            return Ok(KitConfig::default());
        };
        parse_text(&text).with_context(|| format!("invalid {}", path.display()))
    }

    /// Writes the config through `state::safe_write`, keeping comments and unknown keys. The caller
    /// holds the run lock. An unchanged config writes nothing.
    pub fn save(&self, ctx: &Ctx, run: &RunId) -> Result<()> {
        let path = config_path(&ctx.dirs);
        let old = read_text(&path)?.unwrap_or_default();
        let mut text = toml_set(&old, &["preset"], toml_edit::value(self.preset.as_str()))?;
        text = match self.voice {
            Some(voice) => toml_set(&text, &["voice"], toml_edit::value(voice.as_str()))?,
            None => remove_key(&text, &["voice"])?,
        };
        text = match self.compact_at {
            Some(n) => {
                let n = i64::try_from(n).context("compact_at is too large to store")?;
                toml_set(&text, &["compact_at"], toml_edit::value(n))?
            }
            None => remove_key(&text, &["compact_at"])?,
        };
        for role in Role::ALL {
            let key = ["roles", role.key()];
            text = match self.roles.get(&role) {
                Some(setting) => {
                    toml_set(&text, &key, toml_edit::value(setting.to_config_string()))?
                }
                None => remove_key(&text, &key)?,
            };
        }
        if text == old {
            return Ok(());
        }
        safe_write(ctx, run, &path, &text)
    }
}

/// The setting a role actually runs with: its own entry, else the preset's tier. Escalation and
/// final review sit one tier above the implementer and the reviewer, capped at `smarter`.
pub fn effective(config: &KitConfig, role: Role) -> RoleSetting {
    if let Some(setting) = config.roles.get(&role) {
        return setting.clone();
    }
    let (planner, implementer, reviewer) = config.preset.tiers();
    match role {
        Role::Planner => RoleSetting::Tier(planner),
        Role::Implementer => RoleSetting::Tier(implementer),
        Role::Reviewer => RoleSetting::Tier(reviewer),
        Role::Escalation => {
            RoleSetting::Tier(tier_of(&effective(config, Role::Implementer)).one_up())
        }
        Role::FinalReview => {
            RoleSetting::Tier(tier_of(&effective(config, Role::Reviewer)).one_up())
        }
    }
}

/// Parses a token count: `256k` is 256000, `1m` is 1000000, a plain integer is itself. Zero is an error.
pub fn parse_compact(text: &str) -> Result<u64> {
    let lower = text.trim().to_ascii_lowercase();
    let (digits, scale) = if let Some(digits) = lower.strip_suffix('k') {
        (digits, 1_000)
    } else if let Some(digits) = lower.strip_suffix('m') {
        (digits, 1_000_000)
    } else {
        (lower.as_str(), 1)
    };
    let n: u64 = digits
        .parse()
        .with_context(|| format!("`{text}` is not a token count; try 256k, 1m or 256000"))?;
    if n == 0 {
        bail!("compact_at must be above zero");
    }
    n.checked_mul(scale)
        .ok_or_else(|| anyhow!("`{text}` is too large"))
}

fn config_path(dirs: &KitDirs) -> PathBuf {
    dirs.config.join(CONFIG_FILE)
}

/// Reads a file, or `None` when it does not exist.
fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn parse_text(text: &str) -> Result<KitConfig> {
    let mut config = KitConfig::default();
    if let Some(word) = read_str(text, &["preset"])? {
        config.preset = Preset::parse(&word)?;
    }
    if let Some(word) = read_str(text, &["voice"])? {
        config.voice = Some(Voice::parse(&word)?);
    }
    if let Some(item) = toml_get(text, &["compact_at"])? {
        let n = item
            .as_integer()
            .ok_or_else(|| anyhow!("`compact_at` must be an integer"))?;
        let n = u64::try_from(n)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow!("`compact_at` must be a positive integer"))?;
        config.compact_at = Some(n);
    }
    if toml_get(text, &["roles"])?.is_some_and(|item| item.as_table_like().is_none()) {
        bail!("`roles` must be a table");
    }
    for role in Role::ALL {
        if let Some(word) = read_str(text, &["roles", role.key()])? {
            let setting =
                RoleSetting::parse(&word).with_context(|| format!("roles.{}", role.key()))?;
            config.roles.insert(role, setting);
        }
    }
    Ok(config)
}

fn read_str(text: &str, path: &[&str]) -> Result<Option<String>> {
    let Some(item) = toml_get(text, path)? else {
        return Ok(None);
    };
    let value = item
        .as_str()
        .ok_or_else(|| anyhow!("`{}` must be a string", path.join(".")))?;
    Ok(Some(value.to_string()))
}

/// Removes the key at `path` (a top-level key, or `table.key`). A table left empty is dropped too.
fn remove_key(text: &str, path: &[&str]) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("invalid TOML")?;
    let root = doc.as_table_mut();
    match path {
        [key] => {
            root.remove(key);
        }
        [table, key] => {
            if let Some(child) = root.get_mut(table).and_then(Item::as_table_like_mut) {
                child.remove(key);
                if child.is_empty() {
                    root.remove(table);
                }
            }
        }
        _ => bail!("unsupported config path {path:?}"),
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::roles::{ModelSpec, Tier};

    fn pin(text: &str) -> RoleSetting {
        RoleSetting::Pin(ModelSpec::parse(text).unwrap())
    }

    fn with_implementer(tier: Tier) -> KitConfig {
        let mut config = KitConfig::default();
        config
            .roles
            .insert(Role::Implementer, RoleSetting::Tier(tier));
        config
    }

    fn with_reviewer(tier: Tier) -> KitConfig {
        let mut config = KitConfig::default();
        config.roles.insert(Role::Reviewer, RoleSetting::Tier(tier));
        config
    }

    #[test]
    fn pin_beats_explicit_tier_beats_preset() {
        let mut config = KitConfig::default();
        assert_eq!(
            effective(&config, Role::Planner),
            RoleSetting::Tier(Tier::Smarter),
            "the preset supplies the tier"
        );

        config
            .roles
            .insert(Role::Planner, RoleSetting::Tier(Tier::Cheap));
        assert_eq!(
            effective(&config, Role::Planner),
            RoleSetting::Tier(Tier::Cheap),
            "an explicit tier beats the preset"
        );

        config
            .roles
            .insert(Role::Planner, pin("anthropic/claude-sonnet-5-5#high"));
        assert_eq!(
            effective(&config, Role::Planner),
            pin("anthropic/claude-sonnet-5-5#high"),
            "a pin beats the explicit tier"
        );
    }

    #[test]
    fn escalation_is_one_tier_above_the_implementer_capped_at_smarter() {
        for (from, to) in [
            (Tier::Cheap, Tier::Standard),
            (Tier::Standard, Tier::Smarter),
            (Tier::Smarter, Tier::Smarter),
        ] {
            assert_eq!(
                effective(&with_implementer(from), Role::Escalation),
                RoleSetting::Tier(to),
                "escalation from {from:?}"
            );
        }
    }

    #[test]
    fn final_review_is_one_tier_above_the_reviewer_capped_at_smarter() {
        for (from, to) in [
            (Tier::Cheap, Tier::Standard),
            (Tier::Standard, Tier::Smarter),
            (Tier::Smarter, Tier::Smarter),
        ] {
            assert_eq!(
                effective(&with_reviewer(from), Role::FinalReview),
                RoleSetting::Tier(to),
                "final review from {from:?}"
            );
        }
    }

    #[test]
    fn escalation_reads_a_pinned_implementer_by_family() {
        let mut config = KitConfig::default();
        config
            .roles
            .insert(Role::Implementer, pin("anthropic/claude-haiku-5-5"));
        assert_eq!(
            effective(&config, Role::Escalation),
            RoleSetting::Tier(Tier::Standard)
        );
    }

    #[test]
    fn parse_compact_forms() {
        assert_eq!(parse_compact("256k").unwrap(), 256_000);
        assert_eq!(parse_compact("1m").unwrap(), 1_000_000);
        assert_eq!(parse_compact("256000").unwrap(), 256_000);
        assert!(parse_compact("0").is_err());
        assert!(parse_compact("abc").is_err());
    }

    #[test]
    fn load_rejects_the_tier_word_best() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(dir.path());
        fs::create_dir_all(&dirs.config).unwrap();
        fs::write(
            dirs.config.join("config.toml"),
            "[roles]\nplanner = \"best\"\n",
        )
        .unwrap();
        let err = format!("{:#}", KitConfig::load(&dirs).unwrap_err());
        assert!(err.contains("smarter"), "hint missing: {err}");
    }

    #[test]
    fn load_rejects_a_roles_value_that_is_not_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(dir.path());
        fs::create_dir_all(&dirs.config).unwrap();
        fs::write(dirs.config.join("config.toml"), "roles = \"smarter\"\n").unwrap();
        let err = format!("{:#}", KitConfig::load(&dirs).unwrap_err());
        assert!(err.contains("`roles` must be a table"), "{err}");
    }

    #[test]
    fn missing_file_loads_as_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(dir.path());
        assert_eq!(KitConfig::load(&dirs).unwrap(), KitConfig::default());
    }

    #[test]
    fn config_round_trips_and_keeps_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = KitDirs::rooted(dir.path());
        fs::create_dir_all(&dirs.config).unwrap();
        fs::write(
            dirs.config.join("config.toml"),
            "# my notes\nfuture_key = \"kept\"\npreset = \"careful\"\n\n[roles]\n# planner stays\nplanner = \"smarter\"\n",
        )
        .unwrap();

        let loaded = KitConfig::load(&dirs).unwrap();
        assert_eq!(loaded.preset, Preset::Careful);
        assert_eq!(
            loaded.roles.get(&Role::Planner),
            Some(&RoleSetting::Tier(Tier::Smarter))
        );

        let mut edited = loaded.clone();
        edited.voice = Some(Voice::Megacave);
        edited.compact_at = Some(256_000);
        edited.roles.insert(Role::Reviewer, pin("openai/x"));

        let ctx = Ctx::for_test(dir.path(), FakeRunner::new(), FakeNet::new());
        edited.save(&ctx, &RunId::new()).unwrap();

        let text = fs::read_to_string(dirs.config.join("config.toml")).unwrap();
        assert!(text.contains("# my notes"), "{text}");
        assert!(text.contains("future_key = \"kept\""), "{text}");
        assert!(text.contains("# planner stays"), "{text}");
        assert!(
            dirs.backups.exists(),
            "the first save must back up the old file"
        );
        assert_eq!(KitConfig::load(&dirs).unwrap(), edited);

        edited.roles.remove(&Role::Reviewer);
        edited.voice = None;
        edited.save(&ctx, &RunId::new()).unwrap();
        let cleared = KitConfig::load(&dirs).unwrap();
        assert_eq!(cleared.voice, None);
        assert_eq!(cleared.roles.get(&Role::Reviewer), None);
        assert!(
            !fs::read_to_string(dirs.config.join("config.toml"))
                .unwrap()
                .contains("voice")
        );
    }
}
