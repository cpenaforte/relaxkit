//! The Claude Code adapter's plugin files: the manifest, and one slash-command file per CLI subcommand.
//! Each command file is a thin wrapper that passes its arguments to the binary and shows the output.

use std::fs;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use relaxkit::brand;
use relaxkit::cli::Cli;
use relaxkit::sandbox::claude::PACK_COMMANDS;

/// The sentence that a mutating command file must contain.
const OUTSIDE_SANDBOX: &str = "outside the sandbox";

/// The line that every command file ends its body with.
const SHOW_OUTPUT: &str = "Show the output exactly as printed.";

fn commands_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/claude-plugin/commands")
}

/// The CLI subcommands, without clap's `help`.
fn subcommand_names() -> Vec<String> {
    let names: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|cmd| cmd.get_name().to_string())
        .filter(|name| name != "help")
        .collect();
    assert!(!names.is_empty(), "the Cli has no subcommands");
    names
}

fn command_file(name: &str) -> String {
    let path = commands_dir().join(format!("{name}.md"));
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The frontmatter's `key: value` pairs: the lines between the two `---` lines at the top of a file. Double quotes
/// around a value are removed.
fn frontmatter(name: &str, text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "{name}.md does not open with a --- line"
    );
    let mut pairs = Vec::new();
    for line in lines {
        if line == "---" {
            return pairs;
        }
        let (key, value) = line
            .split_once(": ")
            .unwrap_or_else(|| panic!("{name}.md: frontmatter line without `: `: {line}"));
        pairs.push((key.to_string(), value.trim_matches('"').to_string()));
    }
    panic!("{name}.md has no closing --- line for the frontmatter");
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

/// The file after its frontmatter.
fn body(name: &str, text: &str) -> String {
    let mut seen = 0;
    let mut out = Vec::new();
    for line in text.lines() {
        if seen < 2 {
            if line == "---" {
                seen += 1;
            }
            continue;
        }
        out.push(line);
    }
    assert_eq!(seen, 2, "{name}.md has no frontmatter");
    out.join("\n")
}

#[test]
fn plugin_manifest_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters/claude-plugin/.claude-plugin/plugin.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let manifest: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    assert_eq!(manifest["name"], brand::NAME, "{manifest}");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"), "{manifest}");
}

#[test]
fn every_cli_subcommand_has_a_command_file() {
    let names = subcommand_names();
    for name in &names {
        let path = commands_dir().join(format!("{name}.md"));
        assert!(path.is_file(), "{} is missing", path.display());
    }

    // One file per subcommand and no other: no bare-name or alias commands.
    let mut files: Vec<String> = fs::read_dir(commands_dir())
        .expect("the commands directory is readable")
        .map(|entry| {
            entry
                .expect("a directory entry is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    files.sort();
    let mut expected: Vec<String> = names.iter().map(|name| format!("{name}.md")).collect();
    expected.sort();
    assert_eq!(files, expected, "the commands directory holds other files");
}

#[test]
fn command_files_have_frontmatter() {
    for name in subcommand_names() {
        let text = command_file(&name);
        let pairs = frontmatter(&name, &text);
        let description =
            field(&pairs, "description").unwrap_or_else(|| panic!("{name}.md has no description"));
        assert!(
            !description.trim().is_empty(),
            "{name}.md has an empty description"
        );
        assert_eq!(
            field(&pairs, "allowed-tools"),
            Some("Bash(relaxkit:*)"),
            "{name}.md must allow only Bash(relaxkit:*)"
        );
    }
}

#[test]
fn mutating_commands_say_they_run_outside_the_sandbox() {
    assert_eq!(PACK_COMMANDS.len(), 9, "the mutating list changed");
    for name in PACK_COMMANDS {
        let text = command_file(name);
        assert!(
            text.contains(OUTSIDE_SANDBOX),
            "{name}.md does not say it runs outside the sandbox"
        );
    }
    for name in ["status", "usage"] {
        let text = command_file(name);
        assert!(
            !text.contains(OUTSIDE_SANDBOX),
            "{name}.md says it runs outside the sandbox, but {name} stays sandboxed"
        );
    }
}

#[test]
fn command_bodies_pass_the_arguments_and_show_the_output() {
    for name in subcommand_names() {
        let text = command_file(&name);
        let body = body(&name, &text);
        let inline = format!("!`relaxkit {name} $ARGUMENTS`");
        assert!(
            body.lines().any(|line| line.trim() == inline),
            "{name}.md has no inline line `{inline}`"
        );
        assert!(
            body.contains(SHOW_OUTPUT),
            "{name}.md does not say to show the output"
        );
    }
}
