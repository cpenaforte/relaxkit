//! Claude Code, driven through its native `claude` CLI.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{
    Host, HostInfo, InstalledPlugin, edit_guarded, restore_on_failure, run_edit, run_read, snapshot,
};
use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::state::journal::{self, MarketplaceRecord};

pub struct ClaudeHost {
    info: HostInfo,
    /// The directory holding `settings.json` and `CLAUDE.md`.
    dir: PathBuf,
    /// The state file that `claude mcp` and `claude plugin` write (`.claude.json`).
    state: PathBuf,
}

impl ClaudeHost {
    pub fn new(info: HostInfo, dir: PathBuf, state: PathBuf) -> ClaudeHost {
        ClaudeHost { info, dir, state }
    }

    /// Every user file a native Claude command may rewrite: settings, state, and the plugin registry
    /// (spike A2 lists the two registry files as written by `claude plugin`).
    fn edited_files(&self) -> Vec<PathBuf> {
        vec![
            self.dir.join("settings.json"),
            self.state.clone(),
            self.dir.join("plugins").join("known_marketplaces.json"),
            self.dir.join("plugins").join("installed_plugins.json"),
        ]
    }
}

/// The directory Claude Code reads its settings from, and the file that holds its state (`.claude.json`).
/// `CLAUDE_CONFIG_DIR` moves both; without it the state file sits in the home directory.
pub fn claude_paths(home: &Path, config_dir: Option<&OsStr>) -> (PathBuf, PathBuf) {
    match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            let state = dir.join(".claude.json");
            (dir, state)
        }
        None => (home.join(".claude"), home.join(".claude.json")),
    }
}

/// Parses `claude plugin list`. A plugin's name line is `name@marketplace`, optionally followed on the same line by
/// `Version: x`. The real listing puts `Version: x` on the line under the name, so a version is also read from the
/// block lines after a name line, up to the next name line. Lines without an `@` (the header, a "no plugins" notice,
/// the other fields of a block) are skipped.
pub fn parse_plugin_list(text: &str) -> Vec<InstalledPlugin> {
    let mut plugins: Vec<InstalledPlugin> = Vec::new();
    for line in text.lines() {
        let body = line.trim().trim_start_matches('\u{276f}').trim();
        let name_line = body
            .split_whitespace()
            .next()
            .and_then(|spec| spec.split_once('@'));
        if let Some((name, marketplace)) = name_line {
            plugins.push(InstalledPlugin {
                name: name.to_string(),
                marketplace: Some(marketplace.to_string()),
                version: version_in(body),
            });
        } else if let Some(plugin) = plugins.last_mut()
            && plugin.version.is_none()
        {
            // A `Version:` line belongs to the plugin whose name line came before it.
            plugin.version = version_in(body);
        }
    }
    plugins
}

/// The text after `Version:` on a line, up to the next whitespace.
fn version_in(body: &str) -> Option<String> {
    body.split_once("Version:")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(String::from)
}

impl Host for ClaudeHost {
    fn info(&self) -> &HostInfo {
        &self.info
    }

    fn instructions_file(&self, _ctx: &Ctx) -> PathBuf {
        self.dir.join("CLAUDE.md")
    }

    fn config_file(&self, _ctx: &Ctx) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn mcp_add(
        &self,
        ctx: &Ctx,
        name: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<()> {
        // `--env` goes after the name and before `--`, so its variadic value stops at the separator.
        let mut cmd = Cmd::new("claude").args(&["mcp", "add", "--scope", "user", name]);
        for (key, value) in env {
            cmd = cmd.arg("--env").arg(&format!("{key}={value}"));
        }
        cmd = cmd.arg("--");
        for word in argv {
            cmd = cmd.arg(word);
        }
        // Claude refuses to add a name that exists, so an update first clears the old entry. If the add
        // fails, the old entry is written back.
        let files = self.edited_files();
        let saved = snapshot(&files)?;
        restore_on_failure(ctx, &saved, || {
            self.mcp_remove(ctx, name)?;
            run_edit(ctx, &files, &cmd)
        })
    }

    /// Removing an entry that is not there is not an error: the end state is the same.
    fn mcp_remove(&self, ctx: &Ctx, name: &str) -> Result<()> {
        let cmd = Cmd::new("claude").args(&["mcp", "remove", "--scope", "user", name]);
        edit_guarded(ctx, &self.edited_files(), &cmd)?;
        Ok(())
    }

    fn mcp_entry_present(&self, _ctx: &Ctx, name: &str) -> Result<bool> {
        let text = match std::fs::read_to_string(&self.state) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        Ok(crate::edit::jsonc_get(&text, &["mcpServers", name])?.is_some())
    }

    fn plugin_install(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        let cmd = Cmd::new("claude").args(&["plugin", "install"]).arg(spec);
        run_edit(ctx, &self.edited_files(), &cmd)
    }

    fn plugin_update(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        let cmd = Cmd::new("claude").args(&["plugin", "update"]).arg(spec);
        run_edit(ctx, &self.edited_files(), &cmd)
    }

    fn plugin_list(&self, ctx: &Ctx) -> Result<Vec<InstalledPlugin>> {
        let out = run_read(ctx, &Cmd::new("claude").args(&["plugin", "list"]))?;
        Ok(parse_plugin_list(&out.stdout))
    }

    fn marketplace_add(&self, ctx: &Ctx, source: &str) -> Result<()> {
        let cmd = Cmd::new("claude")
            .args(&["plugin", "marketplace", "add"])
            .arg(source);
        run_edit(ctx, &self.edited_files(), &cmd)?;
        if !ctx.dry_run {
            record_added_marketplace(ctx, source)?;
        }
        Ok(())
    }

    fn marketplace_update(&self, ctx: &Ctx) -> Result<()> {
        let cmd = Cmd::new("claude").args(&["plugin", "marketplace", "update"]);
        run_edit(ctx, &self.edited_files(), &cmd)
    }

    fn state_files(&self, ctx: &Ctx) -> Vec<PathBuf> {
        let mut files = self.edited_files();
        files.push(self.instructions_file(ctx));
        files
    }
}

/// Journals the marketplace `source` was added as, under the name Claude lists it by, so undo can name it. A source that
/// the listing does not show under a name is not journaled, and a listing that fails journals nothing.
fn record_added_marketplace(ctx: &Ctx, source: &str) -> Result<()> {
    let listing = match ctx
        .runner
        .run(&Cmd::new("claude").args(&["plugin", "marketplace", "list"]))
    {
        Ok(out) if out.code == 0 => out.stdout,
        _ => return Ok(()),
    };
    let Some(name) = marketplace_name(&listing, source) else {
        return Ok(());
    };
    let record = MarketplaceRecord {
        run: ctx.run.clone(),
        name,
        group: ctx.group.clone(),
    };
    journal::append_marketplace(&ctx.dirs, &record)
}

/// The name `claude plugin marketplace list` shows for a marketplace, found by the source the listing prints on the
/// line under it. `❯ caveman` followed by `Source: GitHub (JuliusBrussee/caveman)` names `caveman` for that source.
fn marketplace_name(listing: &str, source: &str) -> Option<String> {
    let mut name = None;
    for line in listing.lines() {
        let trimmed = line.trim();
        if let Some(listed) = trimmed.strip_prefix("❯ ") {
            name = Some(listed.trim().to_string());
        } else if trimmed.starts_with("Source:") && trimmed.contains(source) {
            return name;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::host::HostKind;

    use crate::host::testing::Writing;
    use crate::net::FakeNet;

    #[test]
    fn a_version_on_the_line_under_the_name_is_read() {
        // The real `claude plugin list` shape: the version is on its own line under the name line (verified 2026-11-22).
        let text = "Installed plugins:\n\n  \u{276f} superpowers@anthropic-plugin-directory\n    Version: 7.0.2-22f0b463e7a5\n    Scope: user\n    Status: \u{2714} enabled\n";
        let plugins = parse_plugin_list(text);
        assert_eq!(plugins.len(), 1, "{plugins:?}");
        assert_eq!(plugins[0].name, "superpowers");
        assert_eq!(
            plugins[0].marketplace.as_deref(),
            Some("anthropic-plugin-directory")
        );
        assert_eq!(plugins[0].version.as_deref(), Some("7.0.2-22f0b463e7a5"));
    }

    #[test]
    fn a_version_on_the_name_line_is_still_read() {
        let plugins = parse_plugin_list("superpowers@anthropic-plugin-directory  Version: 7.0.2\n");
        assert_eq!(plugins[0].version.as_deref(), Some("7.0.2"));
    }

    #[test]
    fn a_version_is_read_from_its_own_plugin_block_only() {
        // The second block has no Version line, so it must not borrow the first block's version.
        let text = "superpowers@one\n    Version: 7.0.2\nsuperpowers@two\n    Scope: user\n";
        let plugins = parse_plugin_list(text);
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].version.as_deref(), Some("7.0.2"));
        assert_eq!(plugins[1].version, None);
    }

    #[test]
    fn a_plugin_without_a_version_line_does_not_borrow_the_next_plugins_version() {
        // The first block has no Version line; the second block's version belongs to the second plugin only.
        let text = "superpowers@one\n    Scope: user\nsuperpowers@two\n    Version: 7.0.3\n";
        let plugins = parse_plugin_list(text);
        assert_eq!(plugins.len(), 2, "{plugins:?}");
        assert_eq!(plugins[0].version, None, "{plugins:?}");
        assert_eq!(plugins[1].version.as_deref(), Some("7.0.3"), "{plugins:?}");
    }
    fn host(root: &Path) -> ClaudeHost {
        ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/bin/claude"),
                version: "2.1.330".into(),
            },
            root.join(".claude"),
            root.join(".claude.json"),
        )
    }

    fn ctx(root: &Path, fake: &FakeRunner) -> Ctx {
        Ctx::for_test(root, fake.clone(), FakeNet::new())
    }

    fn serena_argv() -> Vec<String> {
        "serena start-mcp-server --context claude-code --project-from-cwd --enable-web-dashboard False"
            .split(' ')
            .map(String::from)
            .collect()
    }

    #[test]
    fn claude_mcp_add_argv() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude mcp", Output::ok(""));
        host(root.path())
            .mcp_add(&ctx(root.path(), &fake), "serena", &serena_argv(), &[])
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "claude mcp remove --scope user serena",
                "claude mcp add --scope user serena -- serena start-mcp-server --context claude-code --project-from-cwd --enable-web-dashboard False",
            ]
        );
    }

    #[test]
    fn claude_mcp_add_passes_env_before_the_separator() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude mcp", Output::ok(""));
        let env = vec![("SERENA_USAGE_REPORTING".to_string(), "false".to_string())];
        host(root.path())
            .mcp_add(&ctx(root.path(), &fake), "serena", &["serena".into()], &env)
            .unwrap();
        assert_eq!(
            fake.calls().last().map(String::as_str),
            Some("claude mcp add --scope user serena --env SERENA_USAGE_REPORTING=false -- serena")
        );
    }

    #[test]
    fn claude_mcp_remove_argv() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude mcp remove", Output::ok(""));
        host(root.path())
            .mcp_remove(&ctx(root.path(), &fake), "serena")
            .unwrap();
        assert_eq!(fake.calls(), vec!["claude mcp remove --scope user serena"]);
    }

    #[test]
    fn claude_mcp_remove_of_an_absent_server_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude mcp remove", Output::fail(1, "no such server"));
        host(root.path())
            .mcp_remove(&ctx(root.path(), &fake), "serena")
            .unwrap();
    }

    #[test]
    fn claude_plugin_install_and_marketplace_argv() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude plugin", Output::ok(""));
        let c = ctx(root.path(), &fake);
        let h = host(root.path());
        h.marketplace_add(&c, "JuliusBrussee/caveman").unwrap();
        h.plugin_install(&c, "caveman@caveman").unwrap();
        h.plugin_update(&c, "caveman@caveman").unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "claude plugin marketplace add JuliusBrussee/caveman",
                "claude plugin marketplace list",
                "claude plugin install caveman@caveman",
                "claude plugin update caveman@caveman",
            ]
        );
    }

    #[test]
    fn a_marketplace_add_journals_the_name_claude_lists_it_under() {
        let root = tempfile::tempdir().unwrap();
        let listing = "Configured marketplaces:\n\n  \u{276f} caveman\n    Source: GitHub (JuliusBrussee/caveman)\n";
        let fake = FakeRunner::new()
            .reply("claude plugin marketplace list", Output::ok(listing))
            .reply("claude plugin marketplace add", Output::ok(""));
        let c = ctx(root.path(), &fake);

        host(root.path())
            .marketplace_add(&c, "JuliusBrussee/caveman")
            .unwrap();

        let records = crate::state::journal::read_marketplaces(&c.dirs).unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].name, "caveman");
        assert_eq!(records[0].run, c.run);
    }

    #[test]
    fn claude_plugin_list_parses_the_marketplace() {
        // Spike A2 (claude-md-management, superpowers from claude-plugins-official) and I3 (superpowers
        // from anthropic-plugin-directory), as printed by Claude Code 2.1.330.
        let sample = "Installed plugins:\n\
            \u{20}\u{20}\u{20}\u{20}\u{276f} claude-md-management@claude-plugins-official  Version: 1.0.0  Scope: user  Status: \u{2714} enabled\n\
            \u{20}\u{20}\u{20}\u{20}\u{276f} superpowers@anthropic-plugin-directory   Version: 7.0.2-22f0b463e7a5   Scope: user   Status: \u{2714} enabled\n";
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("claude plugin list", Output::ok(sample));
        let plugins = host(root.path())
            .plugin_list(&ctx(root.path(), &fake))
            .unwrap();
        assert_eq!(
            plugins,
            vec![
                InstalledPlugin {
                    name: "claude-md-management".into(),
                    marketplace: Some("claude-plugins-official".into()),
                    version: Some("1.0.0".into()),
                },
                InstalledPlugin {
                    name: "superpowers".into(),
                    marketplace: Some("anthropic-plugin-directory".into()),
                    version: Some("7.0.2-22f0b463e7a5".into()),
                },
            ]
        );
    }

    #[test]
    fn claude_paths_follow_config_dir_env() {
        let home = Path::new("/h");
        assert_eq!(
            claude_paths(home, None),
            (
                PathBuf::from("/h/.claude"),
                PathBuf::from("/h/.claude.json")
            )
        );
        let custom = OsStr::new("/c");
        assert_eq!(
            claude_paths(home, Some(custom)),
            (PathBuf::from("/c"), PathBuf::from("/c/.claude.json"))
        );
    }

    #[test]
    fn claude_dry_run_runs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let mut c = ctx(root.path(), &fake);
        c.dry_run = true;
        host(root.path())
            .plugin_install(&c, "caveman@caveman")
            .unwrap();
        assert!(fake.calls().is_empty());
        assert!(
            c.out
                .lines()
                .iter()
                .any(|l| l.contains("would run claude plugin install"))
        );
    }

    #[test]
    fn claude_plugin_registry_files_are_backed_up() {
        use crate::state::journal;
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join(".claude/plugins");
        fs::create_dir_all(&registry).unwrap();
        fs::write(registry.join("known_marketplaces.json"), "{}").unwrap();
        fs::write(registry.join("installed_plugins.json"), "{}").unwrap();
        let fake = FakeRunner::new().reply("claude plugin", Output::ok(""));
        let c = ctx(root.path(), &fake);
        host(root.path())
            .plugin_install(&c, "caveman@caveman")
            .unwrap();
        let journaled: Vec<PathBuf> = journal::read(&c.dirs)
            .unwrap()
            .into_iter()
            .map(|record| record.file)
            .collect();
        assert!(journaled.contains(&registry.join("known_marketplaces.json")));
        assert!(journaled.contains(&registry.join("installed_plugins.json")));
    }

    #[test]
    fn claude_failed_add_keeps_the_old_entry() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join(".claude.json");
        fs::write(
            &state,
            "{\"mcpServers\":{\"serena\":{\"command\":\"old\"}}}",
        )
        .unwrap();
        let mut c = ctx(root.path(), &FakeRunner::new());
        c.runner = Box::new(Writing {
            inner: FakeRunner::new()
                .reply("claude mcp remove", Output::ok(""))
                .reply("claude mcp add", Output::fail(1, "offline")),
            effects: vec![("claude mcp remove", state.clone(), "{\"mcpServers\":{}}")],
        });
        let result = host(root.path()).mcp_add(&c, "serena", &serena_argv(), &[]);
        assert!(result.is_err());
        assert!(
            fs::read_to_string(&state).unwrap().contains("\"old\""),
            "the old serena entry was lost"
        );
    }

    #[test]
    fn a_marketplace_is_named_by_its_source_in_the_listing() {
        let listing = "Configured marketplaces:\n\n  \u{276f} anthropic-plugin-directory\n    Source: Built in (Anthropic Directory)\n\n  \u{276f} caveman\n    Source: GitHub (JuliusBrussee/caveman)\n\n  \u{276f} relaxkit\n    Source: GitHub (cpenaforte/relaxkit)\n";
        assert_eq!(
            marketplace_name(listing, "JuliusBrussee/caveman").as_deref(),
            Some("caveman")
        );
        assert_eq!(
            marketplace_name(listing, "cpenaforte/relaxkit").as_deref(),
            Some("relaxkit")
        );
        assert_eq!(marketplace_name(listing, "example/absent"), None);
    }
}
