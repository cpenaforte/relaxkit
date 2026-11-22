//! Tiers, roles and model specs, and how a role setting resolves to a Claude Code or OpenCode model.

use std::fmt;

use anyhow::{Result, bail};

/// The three tiers a user can pick. The product never says "best".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Cheap,
    Standard,
    Smarter,
}

/// The five roles the kit assigns a model to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Planner,
    Implementer,
    Reviewer,
    Escalation,
    FinalReview,
}

/// A pinned model: `provider/model[#variant]`, or a bare alias that means `anthropic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub provider: String,
    pub model: String,
    pub variant: Option<String>,
}

/// What a role is set to: a tier, or an exact model pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleSetting {
    Tier(Tier),
    Pin(ModelSpec),
}

impl Tier {
    pub fn parse(word: &str) -> Result<Tier> {
        match word {
            "cheap" => Ok(Tier::Cheap),
            "standard" => Ok(Tier::Standard),
            "smarter" => Ok(Tier::Smarter),
            _ => bail!("unknown tier; use cheap, standard or smarter (smarter is the strongest)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Cheap => "cheap",
            Tier::Standard => "standard",
            Tier::Smarter => "smarter",
        }
    }

    /// The Claude Code alias for this tier.
    pub(crate) fn claude_alias(self) -> &'static str {
        match self {
            Tier::Cheap => "haiku",
            Tier::Standard => "sonnet",
            Tier::Smarter => "opus",
        }
    }

    /// The next tier up, capped at `Smarter`.
    pub fn one_up(self) -> Tier {
        match self {
            Tier::Cheap => Tier::Standard,
            Tier::Standard | Tier::Smarter => Tier::Smarter,
        }
    }
}

impl Role {
    /// Every role, in config-file order.
    pub const ALL: [Role; 5] = [
        Role::Planner,
        Role::Implementer,
        Role::Reviewer,
        Role::Escalation,
        Role::FinalReview,
    ];

    /// The key used for this role under `[roles]` in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Implementer => "implementer",
            Role::Reviewer => "reviewer",
            Role::Escalation => "escalation",
            Role::FinalReview => "final-review",
        }
    }
}

/// The bare names that mean an Anthropic model.
const ANTHROPIC_ALIASES: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

impl ModelSpec {
    /// Parses `provider/model[#variant]`. A bare `claude-*` id or an alias means `anthropic`.
    pub fn parse(text: &str) -> Result<ModelSpec> {
        let text = text.trim();
        let (body, variant) = match text.split_once('#') {
            Some((body, variant)) => (body, Some(variant)),
            None => (text, None),
        };
        if variant.is_some_and(|v| v.is_empty() || v.contains('#')) {
            bail!("`{text}` has an empty or repeated `#variant`");
        }
        let (provider, model) = match body.split_once('/') {
            Some(parts) => parts,
            None if body.starts_with("claude-") || ANTHROPIC_ALIASES.contains(&body) => {
                ("anthropic", body)
            }
            None => bail!(
                "`{text}` is not `provider/model`, a claude-* id, or one of haiku, sonnet, opus, fable"
            ),
        };
        if provider.is_empty()
            || model.is_empty()
            || model.contains('/')
            || text.contains(char::is_whitespace)
        {
            bail!("`{text}` is not `provider/model`");
        }
        Ok(ModelSpec {
            provider: provider.to_string(),
            model: model.to_string(),
            variant: variant.map(str::to_string),
        })
    }
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)?;
        if let Some(variant) = &self.variant {
            write!(f, "#{variant}")?;
        }
        Ok(())
    }
}

impl RoleSetting {
    /// Parses a tier word or a model spec. `best` is rejected with a hint to use `smarter`.
    pub fn parse(text: &str) -> Result<RoleSetting> {
        let text = text.trim();
        match text {
            "cheap" | "standard" | "smarter" | "best" => Ok(RoleSetting::Tier(Tier::parse(text)?)),
            _ => Ok(RoleSetting::Pin(ModelSpec::parse(text)?)),
        }
    }

    /// The text stored in `config.toml`: the tier word, or the spec.
    pub fn to_config_string(&self) -> String {
        match self {
            RoleSetting::Tier(tier) => tier.as_str().to_string(),
            RoleSetting::Pin(spec) => spec.to_string(),
        }
    }
}

/// The tier a setting stands for. A pin takes its tier from the model family in its name.
pub fn tier_of(setting: &RoleSetting) -> Tier {
    match setting {
        RoleSetting::Tier(tier) => *tier,
        RoleSetting::Pin(spec) => {
            let model = spec.model.to_ascii_lowercase();
            if model.contains("haiku") {
                Tier::Cheap
            } else if model.contains("opus") {
                Tier::Smarter
            } else {
                // Sonnet, and anything outside the three families.
                Tier::Standard
            }
        }
    }
}

/// The Claude Code model argument for a setting, or `None` when the setting pins a non-Anthropic model.
pub fn resolve_claude(setting: &RoleSetting) -> Option<String> {
    match setting {
        RoleSetting::Tier(tier) => Some(tier.claude_alias().to_string()),
        RoleSetting::Pin(spec) if spec.provider == "anthropic" => Some(spec.model.clone()),
        RoleSetting::Pin(_) => None,
    }
}

/// The OpenCode model id for a setting. A tier picks the newest Anthropic model of its family in
/// `available`; a pin returns itself, variant included.
pub fn resolve_opencode(setting: &RoleSetting, available: &[String]) -> Option<String> {
    match setting {
        RoleSetting::Pin(spec) => Some(spec.to_string()),
        RoleSetting::Tier(tier) => {
            let family = tier.claude_alias();
            available
                .iter()
                .filter_map(|id| {
                    let model = id.strip_prefix("anthropic/")?;
                    let version = family_version(model, family)?;
                    Some((version, id))
                })
                .fold(
                    None,
                    |best: Option<(Vec<u64>, &String)>, candidate| match best {
                        Some(current) if current.0 >= candidate.0 => Some(current),
                        _ => Some(candidate),
                    },
                )
                .map(|(_, id)| id.clone())
        }
    }
}

/// The numeric version after `family` in `model`, or `None` when `model` is not in that family or has no version.
/// `claude-opus-5-10` gives `[5, 10]`, so 5-10 sorts after 5-9.
pub(crate) fn family_version(model: &str, family: &str) -> Option<Vec<u64>> {
    let model = model
        .split('#')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let at = model.find(family)?;
    // Runs of eight or more digits are date stamps (`20250929`), not version parts.
    let parts = |text: &str| -> Vec<u64> {
        text.split(|c: char| !c.is_ascii_digit())
            .filter(|digits| !digits.is_empty() && digits.len() < 8)
            .filter_map(|digits| digits.parse::<u64>().ok())
            .collect()
    };
    // The version follows the family word (`claude-sonnet-5-5`) or, for old-style ids, precedes it
    // (`claude-3-5-sonnet-20241022`). No digits at all, as in `claude-sonnet-latest`, is no family (F23-20, F23-23).
    let after = parts(&model[at + family.len()..]);
    let version = if after.is_empty() {
        parts(&model[..at])
    } else {
        after
    };
    if version.is_empty() {
        return None;
    }
    Some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(text: &str) -> RoleSetting {
        RoleSetting::Pin(ModelSpec::parse(text).unwrap())
    }

    #[test]
    fn parse_model_spec_forms() {
        let full = ModelSpec::parse("anthropic/claude-haiku-5-5").unwrap();
        assert_eq!(full.provider, "anthropic");
        assert_eq!(full.model, "claude-haiku-5-5");
        assert_eq!(full.variant, None);

        let variant = ModelSpec::parse("anthropic/claude-sonnet-5-5#high").unwrap();
        assert_eq!(variant.variant.as_deref(), Some("high"));

        let alias = ModelSpec::parse("haiku").unwrap();
        assert_eq!(alias.provider, "anthropic");
        assert_eq!(alias.model, "haiku");

        let openai = ModelSpec::parse("openai/x").unwrap();
        assert_eq!(openai.provider, "openai");

        assert!(ModelSpec::parse("bad//").is_err());
        assert!(ModelSpec::parse("gpt-4").is_err());
        assert!(ModelSpec::parse("open ai/x").is_err());
    }

    #[test]
    fn model_spec_display_round_trips() {
        let spec = ModelSpec::parse("anthropic/claude-sonnet-5-5#high").unwrap();
        assert_eq!(spec.to_string(), "anthropic/claude-sonnet-5-5#high");
        assert_eq!(
            ModelSpec::parse(&spec.to_string()).unwrap(),
            spec,
            "display must parse back to the same spec"
        );
    }

    #[test]
    fn pinned_roles_derive_by_family() {
        assert_eq!(tier_of(&pin("anthropic/claude-haiku-5-5")), Tier::Cheap);
        assert_eq!(tier_of(&pin("anthropic/claude-sonnet-5-5")), Tier::Standard);
        assert_eq!(tier_of(&pin("anthropic/claude-opus-5-5")), Tier::Smarter);
        assert_eq!(tier_of(&pin("openai/x")), Tier::Standard);
        assert_eq!(tier_of(&RoleSetting::Tier(Tier::Smarter)), Tier::Smarter);
    }

    #[test]
    fn claude_resolution() {
        assert_eq!(
            resolve_claude(&RoleSetting::Tier(Tier::Cheap)).as_deref(),
            Some("haiku")
        );
        assert_eq!(
            resolve_claude(&RoleSetting::Tier(Tier::Standard)).as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            resolve_claude(&RoleSetting::Tier(Tier::Smarter)).as_deref(),
            Some("opus")
        );
        assert_eq!(
            resolve_claude(&pin("anthropic/claude-haiku-5-5")).as_deref(),
            Some("claude-haiku-5-5")
        );
        assert_eq!(resolve_claude(&pin("sonnet")).as_deref(), Some("sonnet"));
        assert_eq!(resolve_claude(&pin("openai/x")), None);
        assert_eq!(
            resolve_claude(&pin("anthropic/claude-sonnet-5-5#high")).as_deref(),
            Some("claude-sonnet-5-5"),
            "a variant is ignored for Claude Code"
        );
    }

    #[test]
    fn opencode_resolution_picks_the_newest_family_model() {
        let available: Vec<String> = [
            "anthropic/claude-opus-5-1",
            "anthropic/claude-opus-5-5",
            "anthropic/claude-haiku-5-5",
            "openai/foo",
        ]
        .iter()
        .map(|id| id.to_string())
        .collect();
        assert_eq!(
            resolve_opencode(&RoleSetting::Tier(Tier::Smarter), &available).as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(
            resolve_opencode(
                &RoleSetting::Tier(Tier::Smarter),
                &["openai/foo".to_string()]
            ),
            None,
            "no Anthropic models gives None"
        );
        assert_eq!(
            resolve_opencode(&pin("anthropic/claude-sonnet-5-5#high"), &[]).as_deref(),
            Some("anthropic/claude-sonnet-5-5#high"),
            "a pin returns itself, variant included"
        );
    }

    #[test]
    fn opencode_version_compares_numerically() {
        let available: Vec<String> = ["anthropic/claude-opus-5-9", "anthropic/claude-opus-5-10"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(
            resolve_opencode(&RoleSetting::Tier(Tier::Smarter), &available).as_deref(),
            Some("anthropic/claude-opus-5-10")
        );

        let sonnet: Vec<String> = [
            "anthropic/claude-sonnet-4-5-20250929",
            "anthropic/claude-sonnet-4-20250514",
        ]
        .iter()
        .map(|id| id.to_string())
        .collect();
        assert_eq!(
            resolve_opencode(&RoleSetting::Tier(Tier::Standard), &sonnet).as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250929"),
            "an eight-digit date stamp must not outrank a real version"
        );

        let haiku: Vec<String> = [
            "anthropic/claude-3-haiku-20240307",
            "anthropic/claude-haiku-4-5",
        ]
        .iter()
        .map(|id| id.to_string())
        .collect();
        assert_eq!(
            resolve_opencode(&RoleSetting::Tier(Tier::Cheap), &haiku).as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
    }

    #[test]
    fn tier_word_best_is_rejected_with_a_smarter_hint() {
        let err = RoleSetting::parse("best").unwrap_err().to_string();
        assert!(err.contains("smarter"), "hint missing: {err}");
        assert!(
            !err.contains("best"),
            "the product must not say best: {err}"
        );
        assert!(Tier::parse("best").is_err());
        assert_eq!(
            RoleSetting::parse(" cheap ").unwrap(),
            RoleSetting::Tier(Tier::Cheap)
        );
        assert_eq!(
            RoleSetting::parse("smarter").unwrap(),
            RoleSetting::Tier(Tier::Smarter)
        );
    }

    #[test]
    fn a_family_needs_a_version() {
        // F23-20: `claude-sonnet-latest` has no version, so it is not the sonnet family.
        assert_eq!(family_version("claude-sonnet-latest", "sonnet"), None);
        assert_eq!(
            family_version("claude-sonnet-5-5", "sonnet"),
            Some(vec![5, 5])
        );
        // F23-23: an old-style id has its version before the family word, and the date stamp is not a version.
        assert_eq!(
            family_version("claude-3-5-sonnet-20241022", "sonnet"),
            Some(vec![3, 5])
        );
        let listed = ["anthropic/claude-3-5-sonnet-20241022".to_string()];
        assert_eq!(
            resolve_opencode(&RoleSetting::Tier(Tier::Standard), &listed),
            Some("anthropic/claude-3-5-sonnet-20241022".to_string())
        );
    }
}
