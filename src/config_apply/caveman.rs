//! caveman's OpenCode files (ruling T8-2, spike A3, owner decision 2026-11-22). relaxkit copies the files that caveman's
//! installer defines at the latest tag (`installer/install.js`, the opencode install, at v4.1.0) and writes the V2
//! `plugins` entry itself. It never runs caveman's `install.js`, which writes the V1 key `plugin`. Every write goes
//! through `state::safe_write`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::edit::{block_upsert, jsonc_append, jsonc_get};
use crate::host::opencode::plugin_name;
use crate::host::{Host, HostKind};
use crate::state::{RunId, safe_write};

/// The plugin files, as `(source, destination)`. Sources are relative to the repository root, destinations to the
/// OpenCode config directory. The two `src/hooks` files are renamed to `.cjs` on copy, as the installer does.
const PLUGIN_FILES: [(&str, &str); 5] = [
    (
        "src/plugins/opencode/plugin.js",
        "plugins/caveman/plugin.js",
    ),
    (
        "src/plugins/opencode/server.js",
        "plugins/caveman/server.js",
    ),
    (
        "src/plugins/opencode/package.json",
        "plugins/caveman/package.json",
    ),
    (
        "src/hooks/caveman-config.js",
        "plugins/caveman/caveman-config.cjs",
    ),
    (
        "src/hooks/caveman-parse.js",
        "plugins/caveman/caveman-parse.cjs",
    ),
];

/// Command files, copied from `src/plugins/opencode/commands/` to `commands/`.
const COMMAND_FILES: [&str; 8] = [
    "caveman.md",
    "ultracave.md",
    "megacave.md",
    "caveman-commit.md",
    "caveman-review.md",
    "caveman-compress.md",
    "caveman-stats.md",
    "caveman-help.md",
];

/// Subagent files, copied from `agents/` to `agents/` with the OpenCode frontmatter transform.
const AGENT_FILES: [&str; 3] = [
    "cavecrew-investigator.md",
    "cavecrew-builder.md",
    "cavecrew-reviewer.md",
];

/// Skill directories, copied whole from `skills/<name>` to `skills/<name>`.
const SKILL_DIRS: [&str; 9] = [
    "caveman",
    "ultracave",
    "megacave",
    "caveman-commit",
    "caveman-review",
    "caveman-help",
    "caveman-stats",
    "caveman-compress",
    "cavecrew",
];

/// The ruleset for AGENTS.md. Its text becomes the managed block `caveman`, so the user's own text is kept.
const RULES_FILE: &str = "src/rules/caveman-activate.md";

/// The managed block id for caveman's ruleset in AGENTS.md.
const BLOCK_ID: &str = "caveman";

/// The V2 `plugins` entry for the copied plugin, relative to the config directory.
pub(crate) const PLUGIN_SPEC: &str = "./plugins/caveman";

/// Copies caveman's OpenCode files from `source` (the repository root) into the OpenCode config directory, writes the
/// ruleset block into AGENTS.md, and lists the plugin under the V2 `plugins` key. Every owned file is read and checked
/// before anything is written. A second run writes nothing.
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
    let agents_path = host.instructions_file(ctx);
    let dir = agents_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("the OpenCode config directory is not known"))?;

    let (files, rules) = plan(source, &dir)?;
    for file in &files {
        write_unless_same(ctx, run, &file.to, &file.text)?;
    }

    let agents_old = read_text(&agents_path)?.unwrap_or_default();
    let agents_new = block_upsert(&agents_old, BLOCK_ID, rules.trim_end());
    write_unless_same(ctx, run, &agents_path, &agents_new)?;

    add_plugin_entry(ctx, run, host)
}

/// One owned file, read and transformed, with the path it is written to.
struct Planned {
    to: PathBuf,
    text: String,
}

/// Reads every owned file into memory, and the ruleset. Nothing is written here, so a missing, linked or non-text
/// owned file stops the install before any write.
fn plan(source: &Path, dir: &Path) -> Result<(Vec<Planned>, String)> {
    let mut files = Vec::new();
    for (from, to) in PLUGIN_FILES {
        files.push(read_planned(&source.join(from), &dir.join(to), false)?);
    }
    for name in COMMAND_FILES {
        let from = source.join("src/plugins/opencode/commands").join(name);
        files.push(read_planned(
            &from,
            &dir.join("commands").join(name),
            false,
        )?);
    }
    for name in AGENT_FILES {
        let from = source.join("agents").join(name);
        files.push(read_planned(&from, &dir.join("agents").join(name), true)?);
    }
    for name in SKILL_DIRS {
        let from = source.join("skills").join(name);
        collect_tree(&from, &dir.join("skills").join(name), &mut files)?;
    }
    let rules = read_owned(&source.join(RULES_FILE))?;
    Ok((files, rules))
}

/// Reads one owned file as text, transformed for an agent. A missing, linked or non-text file is a named error.
fn read_planned(from: &Path, to: &Path, agent: bool) -> Result<Planned> {
    let text = read_owned(from)?;
    let text = if agent {
        agent_frontmatter(&text)
    } else {
        text
    };
    Ok(Planned {
        to: to.to_path_buf(),
        text,
    })
}

/// The text of one owned file. A missing, linked or non-text file is a named error.
fn read_owned(from: &Path) -> Result<String> {
    owned_metadata(from)?;
    fs::read_to_string(from).with_context(|| {
        format!(
            "caveman's OpenCode file {} is not readable as text; nothing was copied",
            from.display()
        )
    })
}

/// Reads every file under a skill directory, in name order. A symbolic link is refused, as the installer refuses it.
fn collect_tree(from: &Path, to: &Path, files: &mut Vec<Planned>) -> Result<()> {
    let meta = owned_metadata(from)?;
    if !meta.is_dir() {
        bail!(
            "caveman's skill {} is not a directory; nothing was copied",
            from.display()
        );
    }
    let mut children: Vec<_> = fs::read_dir(from)
        .with_context(|| format!("cannot read {}", from.display()))?
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("cannot read {}", from.display()))?;
    children.sort_by_key(|child| child.file_name());
    for child in children {
        let path = child.path();
        let target = to.join(child.file_name());
        if owned_metadata(&path)?.is_dir() {
            collect_tree(&path, &target, files)?;
        } else {
            files.push(read_planned(&path, &target, false)?);
        }
    }
    Ok(())
}

/// The metadata of an owned path, without following a link. A missing path and a link are named errors.
fn owned_metadata(path: &Path) -> Result<fs::Metadata> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => bail!(
            "caveman's OpenCode file {} is missing from the repository archive; nothing was copied",
            path.display()
        ),
        Err(err) => return Err(err).with_context(|| format!("cannot inspect {}", path.display())),
    };
    if meta.file_type().is_symlink() {
        bail!(
            "symbolic link {} refused, as caveman's installer refuses it; nothing was copied",
            path.display()
        );
    }
    Ok(meta)
}

/// Lists the copied plugin under the V2 `plugins` key. An entry already there is kept, and the key is written only
/// when the entry is missing.
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
        .any(|item| plugin_name(item) == Some(PLUGIN_SPEC))
    {
        return Ok(());
    }
    let new = jsonc_append(&old, &["plugins"], &json!(PLUGIN_SPEC))?;
    write_if_changed(ctx, run, &path, &old, &new)
}

/// The file's text, or `None` when the file does not exist.
fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes `new` only when it differs from the file on disk, so an unchanged file is neither backed up nor journaled.
fn write_unless_same(ctx: &Ctx, run: &RunId, path: &Path, new: &str) -> Result<()> {
    if read_text(path)?.as_deref() == Some(new) {
        return Ok(());
    }
    safe_write(ctx, run, path, new)
}

/// Writes `new` only when it differs from `old`, so an unchanged file is neither backed up nor journaled.
fn write_if_changed(ctx: &Ctx, run: &RunId, path: &Path, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    safe_write(ctx, run, path, new)
}

/// The OpenCode form of an agent file: `tools:` lists and provider-less `model:` values are dropped, and
/// `mode: subagent` is added when no mode is set. Ports `transformOpencodeAgentFrontmatter` with `subagent: true`.
pub fn agent_frontmatter(content: &str) -> String {
    const FENCE: &str = "---\n";
    if !content.starts_with(FENCE) {
        return content.to_string();
    }
    let Some(found) = content[FENCE.len()..].find("\n---") else {
        return content.to_string();
    };
    let fm_end = FENCE.len() + found;
    let frontmatter = &content[FENCE.len()..fm_end];
    let rest = &content[fm_end..];

    let mut lines: Vec<&str> = Vec::new();
    let mut dropping = false;
    let mut has_mode = false;
    for line in frontmatter.split('\n') {
        if dropping {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            dropping = false;
        }
        if is_key(line, "tools") {
            dropping = true;
            continue;
        }
        if let Some(model) = model_value(line)
            && provider_less(model)
        {
            continue;
        }
        if is_key(line, "mode") {
            has_mode = true;
        }
        lines.push(line);
    }
    if !has_mode {
        lines.push("mode: subagent");
    }
    format!("{FENCE}{}{rest}", lines.join("\n"))
}

/// Whether `line` is `key:` at the start, with optional spaces or tabs before the colon.
fn is_key(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .map(|rest| rest.trim_start_matches([' ', '\t']))
        .is_some_and(|rest| rest.starts_with(':'))
}

/// The value of a `model:` line. A value with a carriage return is not read, as the installer's pattern does not match it.
fn model_value(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("model")?;
    let rest = rest.trim_start_matches([' ', '\t']).strip_prefix(':')?;
    let value = rest.trim_start_matches([' ', '\t']);
    (!value.contains('\r')).then_some(value)
}

/// Whether a `model:` value has no provider, so OpenCode would read it as `provider/`. A trailing comment and
/// surrounding quotes are ignored first, as the installer does.
fn provider_less(raw: &str) -> bool {
    let value = strip_comment(raw).trim();
    let value = unquoted(value).trim();
    !value.is_empty() && !value.contains('/')
}

/// The text before the first `#` that follows whitespace, with that whitespace removed.
fn strip_comment(raw: &str) -> &str {
    for (at, c) in raw.char_indices() {
        if c == '#' {
            let before = raw[..at].trim_end_matches(char::is_whitespace);
            if before.len() < at {
                return before;
            }
        }
    }
    raw
}

/// The text inside matching single or double quotes, or the value itself.
fn unquoted(value: &str) -> &str {
    for quote in ['\'', '"'] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
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

    const RULES: &str = "Respond terse like smart caveman.\n\nRule two.\n";

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

    fn config_path(root: &Path) -> PathBuf {
        root.join(".config/opencode/opencode.jsonc")
    }

    /// A repository root with every owned file the installer copies at v4.1.0, one body per file.
    fn owned_repo(root: &Path) -> PathBuf {
        let src = root.join("caveman-repo");
        for (source, _) in PLUGIN_FILES {
            write(src.join(source), &format!("// {source}\n"));
        }
        for name in COMMAND_FILES {
            write(
                src.join("src/plugins/opencode/commands").join(name),
                &format!("# {name}\n"),
            );
        }
        for name in AGENT_FILES {
            write(
                src.join("agents").join(name),
                &format!("---\nname: {name}\ntools: [Read, Grep]\nmodel: haiku\n---\nbody\n"),
            );
        }
        for name in SKILL_DIRS {
            write(
                src.join("skills").join(name).join("SKILL.md"),
                &format!("# {name}\n"),
            );
        }
        write(src.join(RULES_FILE), RULES);
        src
    }

    fn dest(root: &Path, relative: &str) -> PathBuf {
        root.join(".config/opencode").join(relative)
    }

    #[test]
    fn owned_files_are_copied_to_the_paths_the_installer_uses() {
        let root = tempfile::tempdir().unwrap();
        write(config_path(root.path()), CONFIG);
        let src = owned_repo(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        for (source, relative) in PLUGIN_FILES {
            assert_eq!(
                fs::read_to_string(dest(root.path(), relative)).unwrap(),
                format!("// {source}\n"),
                "{relative}"
            );
        }
        for name in COMMAND_FILES {
            assert!(
                dest(root.path(), &format!("commands/{name}")).is_file(),
                "{name}"
            );
        }
        for name in SKILL_DIRS {
            assert!(
                dest(root.path(), &format!("skills/{name}/SKILL.md")).is_file(),
                "{name}"
            );
        }
        let agent = fs::read_to_string(dest(root.path(), "agents/cavecrew-builder.md")).unwrap();
        assert!(agent.contains("mode: subagent"), "{agent}");
        assert!(!agent.contains("tools:"), "{agent}");
        assert!(!agent.contains("model: haiku"), "{agent}");
    }

    #[test]
    fn the_plugin_is_listed_under_v2_plugins_and_the_v1_key_is_absent() {
        let root = tempfile::tempdir().unwrap();
        write(config_path(root.path()), CONFIG);
        let src = owned_repo(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

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
    fn a_tuple_entry_that_names_the_plugin_is_not_added_again() {
        // R7-2: `["./plugins/caveman", {}]` names the plugin, so install writes no second entry and the file keeps its
        // bytes.
        let root = tempfile::tempdir().unwrap();
        let config = "{\n  \"plugins\": [[\"./plugins/caveman\", {}]]\n}\n";
        write(config_path(root.path()), config);
        let src = owned_repo(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        assert_eq!(
            fs::read_to_string(config_path(root.path())).unwrap(),
            config
        );
    }

    #[test]
    fn a_second_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        write(config_path(root.path()), CONFIG);
        let src = owned_repo(root.path());
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
    fn agents_md_gets_the_ruleset_as_a_managed_block_and_keeps_user_text() {
        let root = tempfile::tempdir().unwrap();
        write(dest(root.path(), "AGENTS.md"), "My own rules.\n");
        let src = owned_repo(root.path());
        let ctx = ctx(root.path());

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        let agents = fs::read_to_string(dest(root.path(), "AGENTS.md")).unwrap();
        assert!(agents.starts_with("My own rules.\n"), "{agents}");
        assert_eq!(
            block_get(&agents, "caveman"),
            Some(RULES.trim_end().to_string())
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let src = owned_repo(root.path());
        let mut ctx = ctx(root.path());
        ctx.dry_run = true;

        install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap();

        assert!(!root.path().join(".config/opencode").exists());
    }

    #[test]
    fn a_missing_owned_file_is_named_and_nothing_is_written() {
        let root = tempfile::tempdir().unwrap();
        let src = owned_repo(root.path());
        fs::remove_file(src.join("src/plugins/opencode/commands/caveman-help.md")).unwrap();
        let ctx = ctx(root.path());

        let err = format!(
            "{:#}",
            install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap_err()
        );

        assert!(err.contains("caveman-help.md"), "{err}");
        assert!(
            !dest(root.path(), "plugins/caveman/plugin.js").exists(),
            "a missing file must stop before any write"
        );
    }

    #[test]
    fn a_symbolic_link_in_the_owned_set_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let src = owned_repo(root.path());
        let skill = src.join("skills/caveman/SKILL.md");
        fs::remove_file(&skill).unwrap();
        std::os::unix::fs::symlink(src.join(RULES_FILE), &skill).unwrap();
        let ctx = ctx(root.path());

        let err = format!(
            "{:#}",
            install_opencode_files(&ctx, &ctx.run, &opencode(root.path()), &src).unwrap_err()
        );

        assert!(err.contains("symbolic link"), "{err}");
        assert!(!dest(root.path(), "skills/caveman/SKILL.md").exists());
    }

    #[test]
    fn comments_inside_the_plugins_array_survive_the_entry() {
        let root = tempfile::tempdir().unwrap();
        write(
            config_path(root.path()),
            "{\n  \"plugins\": [\n    // keep me\n    \"opencode-sandbox@0.7.0\"\n  ]\n}\n",
        );
        let src = owned_repo(root.path());
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
    fn a_claude_host_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let src = owned_repo(root.path());
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
    fn agent_frontmatter_drops_tools_and_provider_less_models_and_adds_subagent_mode() {
        let input = "---\nname: cavecrew-builder\ntools: [Read, Grep, Bash]\nmodel: haiku\ndescription: x\n---\nbody\n";
        assert_eq!(
            agent_frontmatter(input),
            "---\nname: cavecrew-builder\ndescription: x\nmode: subagent\n---\nbody\n"
        );
    }

    #[test]
    fn agent_frontmatter_drops_a_block_style_tools_list() {
        let input = "---\ntools:\n  - Read\n  - Grep\ndescription: x\n---\nbody\n";
        assert_eq!(
            agent_frontmatter(input),
            "---\ndescription: x\nmode: subagent\n---\nbody\n"
        );
    }

    #[test]
    fn agent_frontmatter_keeps_slash_qualified_models_and_an_existing_mode() {
        let input = "---\nmodel: anthropic/claude-haiku-4-5\nmode: primary\n---\nbody\n";
        assert_eq!(agent_frontmatter(input), input);
    }

    #[test]
    fn agent_frontmatter_drops_a_quoted_provider_less_model_with_a_comment() {
        let input = "---\nmodel: \"haiku\" # cheap\ndescription: x\n---\nbody\n";
        assert_eq!(
            agent_frontmatter(input),
            "---\ndescription: x\nmode: subagent\n---\nbody\n"
        );
    }

    #[test]
    fn agent_frontmatter_leaves_a_file_without_frontmatter_alone() {
        let input = "no frontmatter here\n";
        assert_eq!(agent_frontmatter(input), input);
    }
}
