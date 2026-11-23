//! `relaxkit undo [--run <group>]` (spec 6, ruling K104). Reverts the newest run group, or the named one: the files and
//! link targets it journaled, and the lock as it was before the group (the lock is journaled before it changes, so its
//! journal record holds that state). Host-registry state is not restored; the report says so, names the Claude
//! plugin versions that moved (K86), and names each Claude marketplace the group added with its removal command.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result, anyhow};

use crate::ctx::Ctx;
use crate::state::lockfile::Lockfile;
use crate::state::{RunId, group_of, journal, undo};
use crate::ui::{Mark, detail, outcome};

/// Reverts a group and returns the report lines. `group` is the run group to revert, `None` for the newest.
pub fn run(ctx: &Ctx, group: Option<&str>) -> Result<Vec<String>> {
    let palette = ctx.out.palette();
    let records = journal::read(&ctx.dirs)?;
    let target = match group {
        Some(text) => text
            .parse::<RunId>()
            .with_context(|| format!("`{text}` is not a run id"))?,
        None => match records.last() {
            Some(last) => group_of(last).clone(),
            None => {
                return Ok(vec![outcome(
                    palette,
                    Mark::Skip,
                    "nothing to undo: no changes are journaled",
                )]);
            }
        },
    };
    let members: Vec<_> = records
        .iter()
        .filter(|record| group_of(record) == &target)
        .collect();
    if members.is_empty() {
        return Err(anyhow!("no journaled changes for run group {target}"));
    }
    let runs: BTreeSet<&RunId> = members.iter().map(|record| &record.run).collect();

    // Read before the revert, which removes the snapshots: the lock as it is now, and as the group found it.
    let now = Lockfile::load(ctx)?;
    let before = runs.iter().find_map(|run| snapshot(ctx, run));
    let moved = claude_plugins_moved(now.as_ref(), before.as_ref());

    let touched = undo(ctx, Some(&target))?;
    // A dry run changed nothing, so its lines are plans, not outcomes.
    let (verb, mark) = if ctx.dry_run {
        ("would revert", Mark::Skip)
    } else {
        ("reverted", Mark::Ok)
    };
    let mut lines = vec![outcome(
        palette,
        mark,
        &format!(
            "{verb} run group {target}: {} file{}",
            touched.len(),
            if touched.len() == 1 { "" } else { "s" }
        ),
    )];
    for path in &touched {
        lines.push(detail(palette, mark, &path.display().to_string()));
    }
    lines.push(outcome(
        palette,
        Mark::Warn,
        "not restored: host registry state (the Claude plugin registry, Caveman state and Serena state)",
    ));
    for line in moved {
        lines.push(outcome(palette, Mark::Warn, &line));
    }
    let marketplaces = journal::read_marketplaces(&ctx.dirs)?;
    for line in marketplace_lines(&marketplaces, &target) {
        lines.push(outcome(palette, Mark::Warn, &line));
    }
    Ok(lines)
}

/// The report lines for the marketplaces a group added. Each stays registered, so the line names it and the command
/// that removes it.
fn marketplace_lines(records: &[journal::MarketplaceRecord], target: &RunId) -> Vec<String> {
    records
        .iter()
        .filter(|record| record.group() == target)
        .map(|record| {
            format!(
                "Claude marketplace {} was added by this run and stays registered; remove it with `claude plugin marketplace remove {}`",
                record.name, record.name
            )
        })
        .collect()
}

/// The lock snapshot a run took before it changed anything, when it has one.
fn snapshot(ctx: &Ctx, run: &RunId) -> Option<Lockfile> {
    let path = ctx.dirs.backups.join(run.as_str()).join("lock.toml");
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// The Claude plugins whose version this undo cannot move back: each one on the Claude host whose version differs
/// from the version before the group, or that the group installed (no earlier lock).
fn claude_plugins_moved(now: Option<&Lockfile>, before: Option<&Lockfile>) -> Vec<String> {
    let Some(now) = now else {
        return Vec::new();
    };
    now.tools
        .iter()
        .filter(|(_, tool)| tool.hosts.iter().any(|host| host == "claude"))
        .filter_map(|(name, tool)| {
            let current = tool.version_for("claude");
            match before.map(|lock| lock.tools.get(name)) {
                Some(Some(earlier)) if earlier.version_for("claude") == current => None,
                Some(Some(earlier)) => Some(format!(
                    "Claude plugin {name}: now {current}, {} before this run; the plugin registry is not restored, so install {} by hand if you want it back",
                    earlier.version_for("claude"),
                    earlier.version_for("claude")
                )),
                _ => Some(format!(
                    "Claude plugin {name} {current} was installed by this run; the plugin registry is not restored, so remove it by hand if you want it gone"
                )),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::state::lockfile::LockedTool;

    fn lock(tools: &[(&str, &str, &[&str])]) -> Lockfile {
        Lockfile {
            relaxkit: "0.1.0".into(),
            tools: tools
                .iter()
                .map(|(name, version, hosts)| {
                    (
                        name.to_string(),
                        LockedTool {
                            version: version.to_string(),
                            route: "plugin".into(),
                            hosts: hosts.iter().map(|h| h.to_string()).collect(),
                            host_versions: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_moved_claude_plugin_is_named_and_an_unchanged_one_is_not() {
        let now = lock(&[
            ("superpowers", "6.4.1", &["claude"]),
            ("caveman", "1.1.0", &["claude"]),
            ("rtk", "1.2.1", &["claude"]),
        ]);
        let before = lock(&[
            ("superpowers", "6.4.0", &["claude"]),
            ("caveman", "1.1.0", &["claude"]),
        ]);

        let named = claude_plugins_moved(Some(&now), Some(&before));

        assert_eq!(named.len(), 2, "{named:?}");
        let superpowers = named
            .iter()
            .find(|line| line.contains("superpowers"))
            .unwrap();
        assert!(superpowers.contains("6.4.0"), "{superpowers}");
        let rtk = named.iter().find(|line| line.contains("rtk")).unwrap();
        assert!(rtk.contains("installed by this run"), "{rtk}");
    }

    #[test]
    fn the_marketplaces_a_group_added_are_named_with_their_removal_command() {
        let group = RunId::new();
        let other = RunId::new();
        let records = vec![
            journal::MarketplaceRecord {
                run: group.clone(),
                name: "caveman".into(),
                group: None,
            },
            journal::MarketplaceRecord {
                run: other.clone(),
                name: "neon".into(),
                group: None,
            },
        ];

        let lines = marketplace_lines(&records, &group);

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("caveman"), "{}", lines[0]);
        assert!(
            lines[0].contains("claude plugin marketplace remove caveman"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn opencode_tools_are_not_named_as_claude_plugins() {
        let now = lock(&[("serena", "2.1.0", &["opencode"])]);
        assert!(claude_plugins_moved(Some(&now), None).is_empty());
    }
}
