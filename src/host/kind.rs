//! The two hosts relaxkit supports: Claude Code and OpenCode.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostKind {
    Claude,
    Opencode,
}

impl HostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HostKind::Claude => "claude",
            HostKind::Opencode => "opencode",
        }
    }
}

impl FromStr for HostKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(HostKind::Claude),
            "opencode" => Ok(HostKind::Opencode),
            other => Err(anyhow!(
                "unknown host `{other}`, expected claude or opencode"
            )),
        }
    }
}

impl fmt::Display for HostKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_kind_round_trips() {
        assert_eq!("claude".parse::<HostKind>().unwrap(), HostKind::Claude);
        assert_eq!("opencode".parse::<HostKind>().unwrap(), HostKind::Opencode);
        assert_eq!(HostKind::Claude.to_string(), "claude");
        assert_eq!(HostKind::Opencode.as_str(), "opencode");
        assert!("codex".parse::<HostKind>().is_err());
    }
}
