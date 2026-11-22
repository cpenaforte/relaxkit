//! The three presets (tier triples and a voice) and the voice choices.

use anyhow::{Result, bail};

use crate::roles::Tier;

/// A named bundle of tiers and a voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    Lean,
    #[default]
    Balanced,
    Careful,
}

/// The output style the kit applies to agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    Caveman,
    Ultracave,
    Megacave,
    Off,
}

impl Preset {
    pub fn parse(word: &str) -> Result<Preset> {
        match word {
            "lean" => Ok(Preset::Lean),
            "balanced" => Ok(Preset::Balanced),
            "careful" => Ok(Preset::Careful),
            other => bail!("unknown preset `{other}`; use lean, balanced or careful"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Lean => "lean",
            Preset::Balanced => "balanced",
            Preset::Careful => "careful",
        }
    }

    /// Tiers in the order planner, implementer, reviewer.
    pub fn tiers(self) -> (Tier, Tier, Tier) {
        match self {
            Preset::Lean => (Tier::Standard, Tier::Cheap, Tier::Standard),
            Preset::Balanced => (Tier::Smarter, Tier::Cheap, Tier::Standard),
            Preset::Careful => (Tier::Smarter, Tier::Standard, Tier::Smarter),
        }
    }

    pub fn voice(self) -> Voice {
        match self {
            Preset::Lean => Voice::Ultracave,
            Preset::Balanced => Voice::Caveman,
            Preset::Careful => Voice::Off,
        }
    }
}

impl Voice {
    pub fn parse(word: &str) -> Result<Voice> {
        match word {
            "caveman" => Ok(Voice::Caveman),
            "ultracave" => Ok(Voice::Ultracave),
            "megacave" => Ok(Voice::Megacave),
            "off" => Ok(Voice::Off),
            other => bail!("unknown voice `{other}`; use caveman, ultracave, megacave or off"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Voice::Caveman => "caveman",
            Voice::Ultracave => "ultracave",
            Voice::Megacave => "megacave",
            Voice::Off => "off",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_match_the_spec_table() {
        assert_eq!(
            Preset::Balanced.tiers(),
            (Tier::Smarter, Tier::Cheap, Tier::Standard)
        );
        assert_eq!(Preset::Balanced.voice(), Voice::Caveman);

        assert_eq!(
            Preset::Lean.tiers(),
            (Tier::Standard, Tier::Cheap, Tier::Standard)
        );
        assert_eq!(Preset::Lean.voice(), Voice::Ultracave);

        assert_eq!(
            Preset::Careful.tiers(),
            (Tier::Smarter, Tier::Standard, Tier::Smarter)
        );
        assert_eq!(Preset::Careful.voice(), Voice::Off);
    }

    #[test]
    fn preset_is_balanced_by_default() {
        assert_eq!(Preset::default(), Preset::Balanced);
    }

    #[test]
    fn preset_and_voice_words_round_trip() {
        for preset in [Preset::Lean, Preset::Balanced, Preset::Careful] {
            assert_eq!(Preset::parse(preset.as_str()).unwrap(), preset);
        }
        for voice in [
            Voice::Caveman,
            Voice::Ultracave,
            Voice::Megacave,
            Voice::Off,
        ] {
            assert_eq!(Voice::parse(voice.as_str()).unwrap(), voice);
        }
        assert!(Preset::parse("nope").is_err());
        assert!(Voice::parse("nope").is_err());
    }
}
