//! caveman's OpenCode files (K31, ruling T8-2, spike A3). relaxkit copies the files from caveman's npm package
//! and writes the V2 `plugins` entry itself. It never runs `caveman --only opencode`, which writes the V1 key `plugin`
//! and drops comments. Every write goes through `state::safe_write`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::edit::{block_upsert, jsonc_append, jsonc_get};
use crate::host::{Host, HostKind};
use crate::state::{RunId, safe_write};

/// The entries copied from caveman's OpenCode files, relative to both the source root and the OpenCode config
/// directory. `plugins/caveman` may be a file or a directory; both are copied as trees.
const COPIED: [&str; 4] = ["plugins/caveman", "commands", "agents", "skills"];

/// caveman's AGENTS.md. Its text goes into a managed block, so the user's own AGENTS.md text is kept.
const AGENTS_FILE: &str = "AGENTS.md";

/// The managed block id for caveman's text in AGENTS.md.
const BLOCK_ID: &str = "caveman";

/// The V2 `plugins` entry for the copied plugin, relative to the config directory.
const PLUGIN_SPEC: &str = "./plugins/caveman";

/// The directory that holds caveman's OpenCode files inside its npm package. Spike A3 records the files that
/// `caveman --only opencode` installed, not where they sit inside the package, so the location is not confirmed.
/// Nothing is guessed: until it is, the handler stops here with a named error and copies nothing (K31).
pub fn opencode_source_root(_ctx: &Ctx) -> Result<PathBuf> {
    bail!(
        "caveman_opencode cannot find caveman's OpenCode files in its npm package: the package layout \
         is not confirmed by spike A3 (K31), so nothing was copied and no plugin entry was written"
    )
}

/// Copies caveman's OpenCode files from `source` into the OpenCode config directory, adds caveman's text to
/// AGENTS.md as the managed block `caveman`, and lists the plugin under the V2 `plugins` key. Every entry must
/// exist in `source` before anything is written. A second run writes nothing.
pub fn install_opencode_files(
    ctx: &Ctx,
    run: &RunId,
    host: &dyn Host,
    source: &Path,
) -> Result<()> {
    if host.info().kind != HostKind::Opencode {
        bail!(
            "caveman_opencode only applies to OpenCode, not {}",
            host.info().kind
        );
    }
    let entries: Vec<&str> = COPIED.iter().copied().chain([AGENTS_FILE]).collect();
    for entry in &entries {
        let from = source.join(entry);
        if !from.exists() {
            bail!(
                "caveman's OpenCode files are missing `{entry}` under {}; nothing was written",
                source.display()
            );
        }
    }

    let dir = host
        .instructions_file(ctx)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("the OpenCode config directory is not known"))?;
    for entry in COPIED {
        copy_tree(ctx, run, &source.join(entry), &dir.join(entry))?;
    }

    let agents_source = fs::read_to_string(source.join(AGENTS_FILE))
        .with_context(|| format!("cannot read caveman's {AGENTS_FILE}"))?;
    let agents_path = host.instructions_file(ctx);
    let agents_old = read_text(&agents_path)?.unwrap_or_default();
    let body = agents_source.trim_end_matches('\n');
    let agents_new = block_upsert(&agents_old, BLOCK_ID, body);
    write_if_changed(ctx, run, &agents_path, &agents_old, &agents_new)?;

    add_plugin_entry(ctx, run, host)
}

/// Lists the copied plugin under the V2 `plugins` key. An entry already there is kept, and the key is written
/// only when the entry is missing.
fn add_plugin_entry(ctx: &Ctx, run: &RunId, host: &dyn Host) -> Result<()> {
    let path = host.config_file(ctx);
    let old = read_text(&path)?
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "{}\n".to_string());
    let plugins = match jsonc_get(&old, &["plugins"])? {
        None => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => bail!("plugins in {} is not a list", path.display()),
    };
    if plugins
        .iter()
        .any(|item| item.as_str() == Some(PLUGIN_SPEC))
    {
        return Ok(());
    }
    let new = jsonc_append(&old, &["plugins"], &json!(PLUGIN_SPEC))?;
    write_if_changed(ctx, run, &path, &old, &new)
}

/// Copies a file, or every file under a directory, to `to`. A file whose text is already the same is not written.
fn copy_tree(ctx: &Ctx, run: &RunId, from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        let mut children: Vec<_> = fs::read_dir(from)
            .with_context(|| format!("cannot read {}", from.display()))?
            .collect::<std::io::Result<_>>()
            .with_context(|| format!("cannot read {}", from.display()))?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            copy_tree(ctx, run, &child.path(), &to.join(child.file_name()))?;
        }
        return Ok(());
    }
    let text = fs::read_to_string(from)
        .with_context(|| format!("cannot read caveman's file {}", from.display()))?;
    let old = read_text(to)?;
    write_if_changed(ctx, run, to, old.as_deref().unwrap_or_default(), &text)
}

/// The file's text, or `None` when the file does not exist.
fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes `new` only when it differs from `old`, so an unchanged file is neither backed up nor journaled.
fn write_if_changed(ctx: &Ctx, run: &RunId, path: &Path, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    safe_write(ctx, run, path, new)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;
    use crate::edit::{block_get, jsonc_get};
    use crate::exec::FakeRunner;
    use crate::host::claude::ClaudeHost;
    use crate::host::opencode::OpencodeHost;
    use crate::host::{HostInfo, HostKind};
    use crate::net::FakeNet;
    use crate::state::journal;

    const CONFIG: &str = "{\n  // user comment\n  \"mcp\": {\n    \"legacy\": {\"type\": \"local\", \"command\": [\"old\"]},\n  },\n}\n";

    fn opencode(root: &Path) -> OpencodeHost {
        OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/bin/opencode"),
                version: "2.2.6".into(),
            },
            root.join(".config/opencode"),
        )
    }

    fn ctx(root: &Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    fn write(path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A source tree with the four entries and AGENTS.md, in the shape spike A3 lists.
    fn source(root: &Path) -> PathBuf {
        let src = root.join("caveman-src");
        write(src.join("plugins/caveman"), "// plugin\n");
        write(src.join("commands/caveman.md"), "# caveman command\n");
        write(src.join("agents/cavemanreview.md"), "# reviewer agent\n");
        write(src.join("skills/caveman/SKILL.md"), "# skill\n");
        write(src.join("AGENTS.md"), "Caveman voice rules.\n");
        src
    }

    fn config_path(root: &Path) -> PathBuf {
        root.join(".config/opencode/opencode.jsonc")
    }

    #[test]
    fn opencode_files_are_copied_and_the_plugin_is_listed_under_v2_plugins() {
        let root = tempfile::tempdir().unwrap();
        write(config_path(root.path()), CONFIG);
        let src = source(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        let dir = root.path().join(".config/opencode");
        assert_eq!(
            fs::read_to_string(dir.join("plugins/caveman")).unwrap(),
            "// plugin\n"
        );
        assert!(dir.join("commands/caveman.md").exists());
        assert!(dir.join("agents/cavemanreview.md").exists());
        assert!(dir.join("skills/caveman/SKILL.md").exists());
        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(text.contains("// user comment"), "{text}");
        assert_eq!(
            jsonc_get(&text, &["plugins"]).unwrap(),
            Some(json!(["./plugins/caveman"]))
        );
        assert_eq!(jsonc_get(&text, &["plugin"]).unwrap(), None, "no V1 key");
        assert!(
            jsonc_get(&text, &["mcp", "legacy"]).unwrap().is_some(),
            "the V1 mcp entry survives"
        );
    }

    #[test]
    fn a_second_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        write(config_path(root.path()), CONFIG);
        let src = source(root.path());
        let host = opencode(root.path());
        let ctx = ctx(root.path());
        install_opencode_files(&ctx, &ctx.run, &host, &src).unwrap();
        let after_first = fs::read_to_string(config_path(root.path())).unwrap();
        let journaled = journal::read(&ctx.dirs).unwrap().len();

        install_opencode_files(&ctx, &RunId::new(), &host, &src).unwrap();

        assert_eq!(
            fs::read_to_string(config_path(root.path())).unwrap(),
            after_first
        );
        assert_eq!(journal::read(&ctx.dirs).unwrap().len(), journaled);
    }

    #[test]
    fn agents_md_gets_a_managed_block_and_keeps_user_text() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path().join(".config/opencode/AGENTS.md"),
            "My own rules.\n",
        );
        let src = source(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        let agents = fs::read_to_string(root.path().join(".config/opencode/AGENTS.md")).unwrap();
        assert!(agents.starts_with("My own rules.\n"), "{agents}");
        assert_eq!(
            block_get(&agents, "caveman"),
            Some("Caveman voice rules.".to_string())
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let src = source(root.path());
        let mut ctx = ctx(root.path());
        ctx.dry_run = true;

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        assert!(!root.path().join(".config/opencode").exists());
    }

    #[test]
    fn a_missing_source_entry_is_named_and_nothing_is_written() {
        let root = tempfile::tempdir().unwrap();
        let src = source(root.path());
        fs::remove_dir_all(src.join("commands")).unwrap();
        let ctx = ctx(root.path());

        let err = format!(
            "{:#}",
            install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap_err()
        );

        assert!(err.contains("commands"), "{err}");
        assert!(
            !root
                .path()
                .join(".config/opencode/plugins/caveman")
                .exists(),
            "a missing entry must stop before any write"
        );
    }

    #[test]
    fn a_claude_host_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let src = source(root.path());
        let claude = ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/bin/claude"),
                version: "2.1.330".into(),
            },
            root.path().join(".claude"),
            root.path().join(".claude.json"),
        );
        let ctx = ctx(root.path());

        let err = format!(
            "{:#}",
            install_opencode_files(&ctx, &ctx.run, &claude, &src).unwrap_err()
        );

        assert!(err.contains("OpenCode"), "{err}");
    }

    #[test]
    fn comments_inside_the_plugins_array_survive_the_entry() {
        let root = tempfile::tempdir().unwrap();
        write(
            config_path(root.path()),
            "{\n  \"plugins\": [\n    // keep me\n    \"opencode-sandbox@0.7.0\"\n  ]\n}\n",
        );
        let src = source(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        let text = fs::read_to_string(config_path(root.path())).unwrap();
        assert!(text.contains("// keep me"), "{text}");
        assert_eq!(
            jsonc_get(&text, &["plugins"]).unwrap(),
            Some(json!(["opencode-sandbox@0.7.0", "./plugins/caveman"]))
        );
    }

    #[test]
    fn the_source_root_is_an_open_question_with_a_named_error() {
        let root = tempfile::tempdir().unwrap();
        let err = format!("{:#}", opencode_source_root(&ctx(root.path())).unwrap_err());

        assert!(err.contains("caveman_opencode"), "{err}");
        assert!(err.contains("K31"), "{err}");
    }
}
