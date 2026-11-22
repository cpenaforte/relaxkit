//! `lock.toml`: the exact tool versions relaxkit installed. Ranges and `latest` are refused.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::state::RunId;
use crate::state::safe_write;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub relaxkit: String,
    #[serde(default)]
    pub tools: BTreeMap<String, LockedTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedTool {
    pub version: String,
    pub route: String,
    pub hosts: Vec<String>,
    /// The version of each host that differs from `version`, so a partial update keeps the other host's version.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host_versions: BTreeMap<String, String>,
}

impl LockedTool {
    /// The version this tool is at on `host`: its own entry in `host_versions`, else `version`.
    pub fn version_for(&self, host: &str) -> &str {
        self.host_versions
            .get(host)
            .map(String::as_str)
            .unwrap_or(&self.version)
    }

    /// The entry after `updated` hosts moved to `version` by `route`. Every other host keeps the version it had, so a
    /// partial update never moves a host it did not touch (ruling K73). `version` stays the primary value, and only
    /// hosts that differ from it are listed in `host_versions`.
    pub fn moved(&self, updated: &[String], version: &str, route: &str) -> LockedTool {
        let mut hosts: BTreeSet<String> = self.hosts.iter().cloned().collect();
        hosts.extend(updated.iter().cloned());
        let host_versions = hosts
            .iter()
            .filter_map(|host| {
                let at = if updated.contains(host) {
                    version.to_string()
                } else {
                    self.version_for(host).to_string()
                };
                (at != version).then(|| (host.clone(), at))
            })
            .collect();
        LockedTool {
            version: version.to_string(),
            route: route.to_string(),
            hosts: hosts.into_iter().collect(),
            host_versions,
        }
    }
}

/// Accepts an exact version, optionally with a leading `v`. Rejects ranges and `latest`.
pub fn check_version(version: &str) -> Result<()> {
    let bare = version.strip_prefix('v').unwrap_or(version);
    semver::Version::parse(bare).map(|_| ()).map_err(|err| {
        anyhow!("`{version}` is not an exact version such as 0.41.0 or v0.41.0 ({err})")
    })
}

fn lock_path(ctx: &Ctx) -> PathBuf {
    ctx.dirs.config.join("lock.toml")
}

impl Lockfile {
    /// Reads `dirs.config/lock.toml`. `None` when the file does not exist yet.
    pub fn load(ctx: &Ctx) -> Result<Option<Lockfile>> {
        let path = lock_path(ctx);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
        };
        let lock: Lockfile =
            toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        lock.validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(Some(lock))
    }

    /// Checks every tool version. Only exact versions may be stored.
    fn validate(&self) -> Result<()> {
        for (name, tool) in &self.tools {
            check_version(&tool.version).with_context(|| format!("tool `{name}`"))?;
        }
        Ok(())
    }

    /// Writes the lockfile through `safe_write`, so `undo` can revert it.
    pub fn save(&self, ctx: &Ctx, run: &RunId) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self).context("cannot render lock.toml")?;
        safe_write(ctx, run, &lock_path(ctx), &text)
    }

    /// Writes this lockfile to `dirs.backups/<run>/lock.toml` and returns that path.
    pub fn snapshot(&self, ctx: &Ctx, run: &RunId) -> Result<PathBuf> {
        let dir = ctx.dirs.backups.join(run.as_str());
        let path = dir.join("lock.toml");
        if ctx.dry_run {
            ctx.out.info(&format!("would write {}", path.display()));
            return Ok(path);
        }
        self.validate()?;
        let text = toml::to_string_pretty(self).context("cannot render lock.toml")?;
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;

    fn tool(version: &str, hosts: &[&str]) -> LockedTool {
        LockedTool {
            version: version.to_string(),
            route: "npm".to_string(),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            host_versions: BTreeMap::new(),
        }
    }

    #[test]
    fn a_partial_move_keeps_the_other_host_on_its_version() {
        let before = tool("7.0.2", &["claude", "opencode"]);
        let moved = before.moved(&["claude".to_string()], "7.0.3", "claude-plugin:m");
        assert_eq!(moved.version, "7.0.3");
        assert_eq!(moved.version_for("claude"), "7.0.3");
        assert_eq!(moved.version_for("opencode"), "7.0.2");
        assert_eq!(
            moved.hosts,
            vec!["claude".to_string(), "opencode".to_string()]
        );
        assert_eq!(moved.host_versions.len(), 1);
    }

    #[test]
    fn a_full_move_leaves_no_host_overrides_and_keeps_the_hosts() {
        let before = tool("7.0.2", &["claude"]);
        let moved = before.moved(&["claude".to_string()], "7.0.3", "npm");
        assert!(moved.host_versions.is_empty());
        assert_eq!(moved.version_for("claude"), "7.0.3");
        assert_eq!(moved.route, "npm");
    }

    #[test]
    fn a_host_that_is_not_listed_reads_the_primary_version() {
        assert_eq!(tool("1.0.0", &["claude"]).version_for("opencode"), "1.0.0");
    }

    fn sample() -> Lockfile {
        let mut tools = BTreeMap::new();
        tools.insert(
            "nvm".to_string(),
            LockedTool {
                version: "v0.41.0".to_string(),
                route: "github:nvm-sh/nvm".to_string(),
                hosts: vec!["claude".to_string()],
                host_versions: BTreeMap::new(),
            },
        );
        Lockfile {
            relaxkit: "0.1.0".to_string(),
            tools,
        }
    }

    #[test]
    fn lockfile_round_trips_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(dir.path(), FakeRunner::new(), FakeNet::new());
        let lock = sample();
        assert_eq!(Lockfile::load(&ctx).unwrap(), None);

        let run = RunId::new();
        lock.save(&ctx, &run).unwrap();
        assert_eq!(Lockfile::load(&ctx).unwrap(), Some(lock.clone()));

        let snapshot = lock.snapshot(&ctx, &run).unwrap();
        let text = fs::read_to_string(&snapshot).unwrap();
        assert_eq!(toml::from_str::<Lockfile>(&text).unwrap(), lock);
    }

    #[test]
    fn lockfile_rejects_ranges_and_latest() {
        assert!(check_version("^1.2").is_err());
        assert!(check_version("latest").is_err());
        assert!(check_version("v0.41.0").is_ok());
        assert!(check_version("7.0.2").is_ok());

        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(dir.path(), FakeRunner::new(), FakeNet::new());
        let mut lock = sample();
        lock.tools.get_mut("nvm").unwrap().version = "^1.2".to_string();
        assert!(lock.save(&ctx, &RunId::new()).is_err());
        assert!(!ctx.dirs.config.join("lock.toml").exists());
    }
}
