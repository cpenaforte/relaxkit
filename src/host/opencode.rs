//! OpenCode, driven through its native `opencode` CLI and its V2 config keys (`plugins`, `mcp.servers`).

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::{Host, HostInfo, InstalledPlugin, restore_on_failure, run_edit, snapshot};
use crate::ctx::Ctx;
use crate::edit::{jsonc_get, jsonc_remove};
use crate::exec::Cmd;
use crate::state::safe_write;

pub struct OpencodeHost {
    info: HostInfo,
    /// The OpenCode config directory, which holds `opencode.jsonc` or `opencode.json` and `AGENTS.md`.
    dir: PathBuf,
}

impl OpencodeHost {
    pub fn new(info: HostInfo, dir: PathBuf) -> OpencodeHost {
        OpencodeHost { info, dir }
    }

    /// Both config names OpenCode accepts. The native commands may write either.
    fn config_candidates(&self) -> Vec<PathBuf> {
        vec![
            self.dir.join("opencode.jsonc"),
            self.dir.join("opencode.json"),
        ]
    }

    /// The plugin specs under `plugins` in the config file. A missing file has none.
    fn plugin_specs(&self) -> Result<Vec<String>> {
        let path = config_file_in(&self.dir);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
        };
        match jsonc_get(&text, &["plugins"])? {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    plugin_name(item).map(String::from).ok_or_else(|| {
                        anyhow!(
                            "plugins in {} has an entry that is not a string",
                            path.display()
                        )
                    })
                })
                .collect(),
            Some(_) => bail!("plugins in {} is not a list", path.display()),
        }
    }
}

/// The plugin spec a `plugins` entry names. A string is its own spec. A two-element array whose first element is a
/// string is `[spec, options]`, which OpenCode 2.2.6 accepts (ruling R6-2). Any other shape names no spec, so it is
/// `None`. Every reader and writer of `plugins` entries uses this one rule (ruling R7-2).
pub fn plugin_name(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(spec) => Some(spec),
        Value::Array(pair) if pair.len() == 2 => pair[0].as_str(),
        _ => None,
    }
}

/// The OpenCode config directory: `$XDG_CONFIG_HOME/opencode`, or `~/.config/opencode` when unset.
pub fn opencode_dir(home: &Path, xdg: Option<&OsStr>) -> PathBuf {
    match xdg.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("opencode"),
        None => home.join(".config").join("opencode"),
    }
}

/// The config file in `dir`: `opencode.jsonc` when it exists, otherwise `opencode.json`. With neither file,
/// `opencode plugin add` creates `opencode.json` (spike B2), so that is the default.
pub fn config_file_in(dir: &Path) -> PathBuf {
    let jsonc = dir.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("opencode.json")
    }
}

/// The package part of a plugin spec: `name` for `name@version`, `@scope/name` for scoped specs.
pub fn package_name(spec: &str) -> &str {
    let offset = usize::from(spec.starts_with('@'));
    match spec[offset..].find('@') {
        Some(at) => &spec[..offset + at],
        None => spec,
    }
}

/// Reads one `plugins` entry as a plugin. The version is whatever follows the package name, except that a git spec
/// names its release after the last `#`: `#v7.0.2` is version 7.0.2 (ruling R8-1).
fn to_plugin(spec: &str) -> InstalledPlugin {
    let name = package_name(spec);
    let version = match spec.rsplit_once('#') {
        Some((_, tag)) => Some(tag.strip_prefix('v').unwrap_or(tag).to_string()),
        None => spec[name.len()..].strip_prefix('@').map(String::from),
    };
    InstalledPlugin {
        name: name.to_string(),
        marketplace: None,
        version,
    }
}

impl Host for OpencodeHost {
    fn info(&self) -> &HostInfo {
        &self.info
    }

    fn instructions_file(&self, _ctx: &Ctx) -> PathBuf {
        self.dir.join("AGENTS.md")
    }

    fn config_file(&self, _ctx: &Ctx) -> PathBuf {
        config_file_in(&self.dir)
    }

    fn mcp_add(
        &self,
        ctx: &Ctx,
        name: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<()> {
        let mut cmd = Cmd::new("opencode").args(&["mcp", "add", "--global"]);
        for (key, value) in env {
            cmd = cmd.arg("--env").arg(&format!("{key}={value}"));
        }
        cmd = cmd.arg(name).arg("--");
        for word in argv {
            cmd = cmd.arg(word);
        }
        // Clear the old entry first, so an update really replaces the argv (for example the K9 `--context ide`).
        // If the add fails, the old entry is written back.
        let files = self.config_candidates();
        let saved = snapshot(&files)?;
        restore_on_failure(ctx, &saved, || {
            self.mcp_remove(ctx, name)?;
            run_edit(ctx, &files, &cmd)
        })
    }

    /// Whether `mcp.servers.<name>` is in either config file (ruling R2-1).
    fn mcp_entry_present(&self, _ctx: &Ctx, name: &str) -> Result<bool> {
        for file in self.config_candidates() {
            let text = match fs::read_to_string(&file) {
                Ok(text) => text,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            if jsonc_get(&text, &["mcp", "servers", name])?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Removes `mcp.servers.<name>` with the comment-preserving editor. No native `mcp remove` is known
    /// for OpenCode 2.2.6 (spike A1 covers add only), so this path does not shell out.
    fn mcp_remove(&self, ctx: &Ctx, name: &str) -> Result<()> {
        let path = config_file_in(&self.dir);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
        };
        let edited = jsonc_remove(&text, &["mcp", "servers", name])?;
        if edited != text {
            safe_write(ctx, &ctx.run, &path, &edited)?;
        }
        Ok(())
    }

    fn plugin_install(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        let cmd = Cmd::new("opencode").args(&["plugin", "add"]).arg(spec);
        run_edit(ctx, &self.config_candidates(), &cmd)
    }

    /// Relaxkit reinstalls the exact pinned spec (controller ruling I1) rather than running
    /// `opencode plugin update`. Entries of the same package are removed first, so two versions never
    /// load side by side.
    fn plugin_update(&self, ctx: &Ctx, spec: &str) -> Result<()> {
        let package = package_name(spec);
        let old: Vec<String> = self
            .plugin_specs()?
            .into_iter()
            .filter(|entry| package_name(entry) == package)
            .collect();
        // If the removal or the add fails, the old entries are written back.
        let files = self.config_candidates();
        let saved = snapshot(&files)?;
        restore_on_failure(ctx, &saved, || {
            for entry in &old {
                let cmd = Cmd::new("opencode").args(&["plugin", "remove"]).arg(entry);
                run_edit(ctx, &files, &cmd)?;
            }
            self.plugin_install(ctx, spec)
        })
    }

    /// Reads the `plugins` array from the config file. `plugin list` is not used: it needs the managed
    /// service on a fixed port, and `--standalone` did not show loaded plugins (spike A1).
    fn plugin_list(&self, _ctx: &Ctx) -> Result<Vec<InstalledPlugin>> {
        Ok(self
            .plugin_specs()?
            .iter()
            .map(|spec| to_plugin(spec))
            .collect())
    }

    fn marketplace_add(&self, _ctx: &Ctx, source: &str) -> Result<()> {
        bail!("OpenCode has no plugin marketplaces, so `{source}` cannot be added")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::exec::{Cmd, FakeRunner, Output, Runner};
    use crate::host::HostKind;
    use crate::net::FakeNet;

    fn host(root: &Path) -> OpencodeHost {
        OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/bin/opencode"),
                version: "2.2.6".into(),
            },
            root.join(".config/opencode"),
        )
    }

    fn ctx(root: &Path, runner: Box<dyn Runner>) -> Ctx {
        let mut c = Ctx::for_test(root, FakeRunner::new(), FakeNet::new());
        c.runner = runner;
        c
    }

    fn config_path(root: &Path) -> PathBuf {
        root.join(".config/opencode/opencode.jsonc")
    }

    fn write_config(root: &Path, text: &str) {
        let path = config_path(root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
    }

    fn serena_argv() -> Vec<String> {
        "serena start-mcp-server --context ide --project-from-cwd --enable-web-dashboard False"
            .split(' ')
            .map(String::from)
            .collect()
    }

    /// Changes the config file's mode whenever `opencode plugin add` runs, as OpenCode does.
    struct ChmodOnPluginAdd {
        inner: FakeRunner,
        file: PathBuf,
    }

    impl Runner for ChmodOnPluginAdd {
        fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
            use std::os::unix::fs::PermissionsExt;
            let out = self.inner.run(cmd)?;
            if cmd.to_string().starts_with("opencode plugin add") {
                fs::set_permissions(&self.file, fs::Permissions::from_mode(0o600))?;
            }
            Ok(out)
        }

        fn which(&self, program: &str) -> Option<PathBuf> {
            self.inner.which(program)
        }
    }

    #[test]
    fn opencode_mcp_add_clears_the_old_entry_first() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  \"mcp\": {\n    \"servers\": {\n      \"serena\": {\"type\": \"local\", \"command\": [\"old\"]}\n    }\n  }\n}\n",
        );
        let fake = FakeRunner::new().reply("opencode mcp add", Output::ok(""));
        host(root.path())
            .mcp_add(
                &ctx(root.path(), Box::new(fake.clone())),
                "serena",
                &serena_argv(),
                &[],
            )
            .unwrap();
        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(!text.contains("\"old\""), "the old serena entry survived");
        assert_eq!(
            fake.calls(),
            vec![
                "opencode mcp add --global serena -- serena start-mcp-server --context ide --project-from-cwd --enable-web-dashboard False"
            ]
        );
    }

    #[test]
    fn opencode_mcp_add_argv() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("opencode mcp add", Output::ok(""));
        let env = vec![("SERENA_USAGE_REPORTING".to_string(), "false".to_string())];
        host(root.path())
            .mcp_add(
                &ctx(root.path(), Box::new(fake.clone())),
                "serena",
                &serena_argv(),
                &env,
            )
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "opencode mcp add --global --env SERENA_USAGE_REPORTING=false serena -- serena start-mcp-server --context ide --project-from-cwd --enable-web-dashboard False"
            ]
        );
    }

    #[test]
    fn opencode_plugin_add_argv() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new().reply("opencode plugin add", Output::ok(""));
        host(root.path())
            .plugin_install(
                &ctx(root.path(), Box::new(fake.clone())),
                "opencode-sandbox@0.7.0",
            )
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec!["opencode plugin add opencode-sandbox@0.7.0"]
        );
    }

    #[test]
    fn opencode_plugin_add_keeps_the_mode_and_backs_up_the_config() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), "{\n  // kept\n  \"plugins\": []\n}\n");
        fs::set_permissions(config_path(root.path()), fs::Permissions::from_mode(0o644)).unwrap();
        let runner = ChmodOnPluginAdd {
            inner: FakeRunner::new().reply("opencode plugin add", Output::ok("")),
            file: config_path(root.path()),
        };
        host(root.path())
            .plugin_install(
                &ctx(root.path(), Box::new(runner)),
                "opencode-sandbox@0.7.0",
            )
            .unwrap();
        let mode = fs::metadata(config_path(root.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
        assert!(root.path().join(".local/share/relaxkit/backups").exists());
    }

    #[test]
    fn opencode_plugin_update_replaces_the_old_entry() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  \"plugins\": [\"opencode-sandbox@0.7.0\", \"other@1.0.0\"]\n}\n",
        );
        let fake = FakeRunner::new()
            .reply("opencode plugin remove", Output::ok(""))
            .reply("opencode plugin add", Output::ok(""));
        host(root.path())
            .plugin_update(
                &ctx(root.path(), Box::new(fake.clone())),
                "opencode-sandbox@0.8.0",
            )
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "opencode plugin remove opencode-sandbox@0.7.0",
                "opencode plugin add opencode-sandbox@0.8.0",
            ]
        );
    }

    #[test]
    fn opencode_plugin_list_reads_the_config_file() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  // comment\n  \"plugins\": [\"opencode-sandbox@0.7.0\", \"./plugins/caveman\",],\n}\n",
        );
        let plugins = host(root.path())
            .plugin_list(&ctx(root.path(), Box::new(FakeRunner::new())))
            .unwrap();
        assert_eq!(
            plugins,
            vec![
                InstalledPlugin {
                    name: "opencode-sandbox".into(),
                    marketplace: None,
                    version: Some("0.7.0".into()),
                },
                InstalledPlugin {
                    name: "./plugins/caveman".into(),
                    marketplace: None,
                    version: None,
                },
            ]
        );
    }

    #[test]
    fn opencode_mcp_remove_edits_only_the_servers_entry() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  // keep me\n  \"mcp\": {\n    \"servers\": {\n      \"serena\": {\"type\": \"local\"},\n      \"other\": {\"type\": \"local\"}\n    },\n  },\n}\n",
        );
        host(root.path())
            .mcp_remove(&ctx(root.path(), Box::new(FakeRunner::new())), "serena")
            .unwrap();
        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(!text.contains("\"serena\""));
        assert!(text.contains("\"other\""));
        assert!(text.contains("// keep me"));
    }

    #[test]
    fn opencode_marketplace_add_is_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let err = host(root.path())
            .marketplace_add(&ctx(root.path(), Box::new(FakeRunner::new())), "x/y")
            .unwrap_err();
        assert!(err.to_string().contains("marketplace"));
    }

    #[test]
    fn opencode_config_location_follows_xdg_and_existing_files() {
        let home = Path::new("/h");
        assert_eq!(
            opencode_dir(home, None),
            PathBuf::from("/h/.config/opencode")
        );
        assert_eq!(
            opencode_dir(home, Some(OsStr::new("/x"))),
            PathBuf::from("/x/opencode")
        );
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("opencode.json"), "{}").unwrap();
        assert_eq!(
            config_file_in(root.path()),
            root.path().join("opencode.json")
        );
        fs::write(root.path().join("opencode.jsonc"), "{}").unwrap();
        assert_eq!(
            config_file_in(root.path()),
            root.path().join("opencode.jsonc")
        );
    }

    #[test]
    fn config_file_defaults_to_json_on_an_empty_home() {
        // Spike B2: `opencode plugin add` on an empty home creates opencode.json.
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            config_file_in(root.path()),
            root.path().join("opencode.json")
        );
    }

    #[test]
    fn package_name_handles_scoped_and_local_specs() {
        assert_eq!(package_name("opencode-sandbox@0.7.0"), "opencode-sandbox");
        assert_eq!(package_name("@scope/pkg@1.2.3"), "@scope/pkg");
        assert_eq!(package_name("@scope/pkg"), "@scope/pkg");
        assert_eq!(package_name("./plugins/caveman"), "./plugins/caveman");
    }

    #[test]
    fn opencode_dry_run_runs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let mut c = ctx(root.path(), Box::new(fake.clone()));
        c.dry_run = true;
        host(root.path())
            .plugin_install(&c, "opencode-sandbox@0.7.0")
            .unwrap();
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn opencode_failed_add_keeps_the_old_entry() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  \"mcp\": {\n    \"servers\": {\n      \"serena\": {\"type\": \"local\", \"command\": [\"old\"]}\n    }\n  }\n}\n",
        );
        let fake = FakeRunner::new().reply("opencode mcp add", Output::fail(1, "offline"));
        let result = host(root.path()).mcp_add(
            &ctx(root.path(), Box::new(fake)),
            "serena",
            &serena_argv(),
            &[],
        );
        assert!(result.is_err());
        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(text.contains("\"old\""), "the old serena entry was lost");
    }

    #[test]
    fn opencode_failed_plugin_update_keeps_the_old_entry() {
        use crate::host::testing::Writing;
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  \"plugins\": [\"opencode-sandbox@0.7.0\"]\n}\n",
        );
        let mut c = ctx(root.path(), Box::new(FakeRunner::new()));
        c.runner = Box::new(Writing {
            inner: FakeRunner::new()
                .reply("opencode plugin remove", Output::ok(""))
                .reply("opencode plugin add", Output::fail(1, "npm fetch failed")),
            effects: vec![(
                "opencode plugin remove",
                config_path(root.path()),
                "{\n  \"plugins\": []\n}\n",
            )],
        });
        let result = host(root.path()).plugin_update(&c, "opencode-sandbox@0.8.0");
        assert!(result.is_err());
        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(
            text.contains("opencode-sandbox@0.7.0"),
            "the old plugin entry was lost"
        );
    }

    #[test]
    fn a_dangling_config_link_stops_the_command_before_it_runs() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".config/opencode");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("nowhere.jsonc"),
            dir.join("opencode.jsonc"),
        )
        .unwrap();
        let fake = FakeRunner::new().reply("opencode plugin add", Output::ok(""));
        let result = host(root.path()).plugin_install(
            &ctx(root.path(), Box::new(fake.clone())),
            "opencode-sandbox@0.7.0",
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("dangling"), "unexpected error: {err}");
        assert!(
            err.contains("opencode.jsonc"),
            "the error must name the link: {err}"
        );
        assert!(
            fake.calls().is_empty(),
            "the command ran through a dangling link"
        );
    }
}
