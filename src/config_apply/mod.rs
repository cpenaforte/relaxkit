//! Writes relaxkit's settings into the hosts' own files: Claude Code's settings and CLAUDE.md, the voice
//! config of caveman, and rtk's display config. Every write goes through `state::safe_write`.

pub mod claude;
pub mod privacy;
pub mod voice;
