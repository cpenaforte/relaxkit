//! Documentation contract checks. The README and the other public Markdown files must match the real `Cli`, link only
//! to targets that exist, carry no personal data, and cite no private working files.
//!
//! The subcommand list comes from the real `Cli`, so a subcommand added to the CLI and missing from the README fails
//! here.

use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use relaxkit::cli::Cli;

fn readme() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md is readable")
}

/// The product description, used by later checks against the CLI, the manifests and the README.
const DESCRIPTION: &str = "A curated pack of agent tools for Claude Code and OpenCode: install, configure, update and diagnose from one CLI.";

/// Public files that still cite private working files. Each must keep citing one of the phrases the cite test
/// checks; once a file is cleaned, the test fails until it leaves this list.
const PENDING_PRIVATE_REFERENCES: &[&str] = &["README.md", "docs/acceptance.md"];

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Reads a file relative to the crate root.
fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_path(rel)).unwrap_or_else(|err| panic!("{rel} is readable: {err}"))
}

fn exists(rel: &str) -> bool {
    repo_path(rel).exists()
}

/// The public Markdown files that exist: the community and release files at the root, then every `docs/*.md`.
fn public_markdown() -> Vec<String> {
    let mut files: Vec<String> = [
        "README.md",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "CODE_OF_CONDUCT.md",
        "AGENTS.md",
        ".github/pull_request_template.md",
    ]
    .into_iter()
    .filter(|rel| exists(rel))
    .map(String::from)
    .collect();
    let mut docs: Vec<String> = std::fs::read_dir(repo_path("docs"))
        .expect("docs/ is readable")
        .map(|entry| {
            let name = entry.expect("docs/ entry is readable").file_name();
            name.to_string_lossy().into_owned()
        })
        .filter(|name| name.ends_with(".md") && repo_path(&format!("docs/{name}")).is_file())
        .map(|name| format!("docs/{name}"))
        .collect();
    docs.sort();
    files.extend(docs);
    files
}

/// GitHub's heading anchor: lowercase, alphanumerics, `-` and `_` kept, spaces become `-`, the rest dropped.
fn github_anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The lines of `markdown` outside fenced code blocks.
fn unfenced_lines(markdown: &str) -> Vec<&str> {
    let mut in_fence = false;
    let mut lines = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            lines.push(line);
        }
    }
    lines
}

/// `(path, anchor)` of every inline `[text](target)` outside fenced code. Web and mail links are skipped.
fn relative_links(markdown: &str) -> Vec<(String, Option<String>)> {
    let mut links = Vec::new();
    for line in unfenced_lines(markdown) {
        let mut rest = line;
        while let Some(open) = rest.find("](") {
            let after = &rest[open + 2..];
            let Some(close) = after.find(')') else {
                break;
            };
            let target = &after[..close];
            rest = &after[close + 1..];
            if target.starts_with("http:")
                || target.starts_with("https:")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let (path, anchor) = match target.split_once('#') {
                Some((path, anchor)) => (path, Some(anchor.to_string())),
                None => (target, None),
            };
            links.push((path.to_string(), anchor));
        }
    }
    links
}

/// The commands of every `sh` fenced block, with a leading `$ ` and a trailing ` # comment` removed.
fn sh_examples(markdown: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let mut in_sh_block = false;
    for line in markdown.lines() {
        let line = line.trim();
        if let Some(info) = line.strip_prefix("```") {
            in_sh_block = !in_sh_block && info.split_whitespace().next() == Some("sh");
            continue;
        }
        if !in_sh_block {
            continue;
        }
        let command = line.strip_prefix("$ ").unwrap_or(line);
        let command = match command.find(" # ") {
            Some(at) => &command[..at],
            None => command,
        };
        let command = command.trim();
        if !command.is_empty() {
            examples.push(command.to_string());
        }
    }
    examples
}

#[test]
fn readme_mentions_every_subcommand() {
    let readme = readme();
    let names: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|cmd| cmd.get_name().to_string())
        .collect();
    assert!(!names.is_empty(), "the Cli has no subcommands");
    for name in names {
        let form = format!("relaxkit {name}");
        assert!(
            readme.contains(&form),
            "README.md does not mention the subcommand `{form}`"
        );
    }
}

fn commands_reference() -> String {
    read("docs/commands.md")
}

/// The text between the heading `## relaxkit --help` and the next fenced block's closing fence.
fn help_block_under(doc: &str, heading: &str) -> String {
    let (_, after) = doc
        .split_once(heading)
        .unwrap_or_else(|| panic!("docs/commands.md has no heading `{heading}`"));
    let mut in_block = false;
    let mut block = Vec::new();
    for line in after.lines() {
        if !in_block {
            in_block = line.starts_with("```");
            continue;
        }
        if line.starts_with("```") {
            break;
        }
        block.push(line);
    }
    block.join("\n")
}

/// The lines with trailing whitespace removed, and no trailing blank lines.
fn trim_lines(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// Every long flag of `cmd` and of its nested subcommands, such as `config set --host`.
fn long_flags(cmd: &clap::Command) -> Vec<String> {
    let mut flags: Vec<String> = cmd
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .collect();
    for sub in cmd.get_subcommands() {
        flags.extend(long_flags(sub));
    }
    flags
}

#[test]
fn commands_reference_covers_every_subcommand_flag_and_slash_command() {
    let doc = commands_reference();
    let cli = Cli::command();
    for sub in cli.get_subcommands().filter(|cmd| cmd.get_name() != "help") {
        let name = sub.get_name();
        let heading = format!("## relaxkit {name}");
        assert!(
            doc.lines().any(|line| line == heading),
            "docs/commands.md has no heading `{heading}`"
        );
        let slash = format!("/relaxkit:{name}");
        assert!(
            doc.contains(&slash),
            "docs/commands.md does not mention `{slash}`"
        );
        for flag in long_flags(sub) {
            assert!(
                doc.contains(&flag),
                "docs/commands.md does not document `{flag}` of `relaxkit {name}`"
            );
        }
    }
    for flag in long_flags(&cli) {
        assert!(
            doc.contains(&flag),
            "docs/commands.md does not document the global `{flag}`"
        );
    }
}

#[test]
fn commands_reference_help_block_matches_the_binary() {
    let doc = commands_reference();
    let block = trim_lines(&help_block_under(&doc, "## relaxkit --help"));
    let expected = trim_lines(&Cli::command().render_help().to_string());
    assert_eq!(
        block, expected,
        "the `## relaxkit --help` block differs from the binary's help text"
    );
}

#[test]
fn commands_reference_documents_the_slash_commands() {
    let doc = commands_reference();
    for form in [
        "/relaxkit:config set implementer cheap",
        "#high",
        "--host",
        "--project",
        "not yet confirmed in a live Claude Code session",
    ] {
        assert!(
            doc.contains(form),
            "docs/commands.md does not show `{form}`"
        );
    }
}

#[test]
fn readme_states_the_platform_scope_and_the_windows_future_note() {
    let readme = readme();
    assert!(
        readme.contains("Linux and macOS"),
        "README.md does not state the platforms"
    );
    assert!(
        readme.contains("Windows"),
        "README.md does not mention Windows"
    );
    assert!(
        readme.contains("possible future addition"),
        "README.md does not call Windows a possible future addition"
    );
    assert!(
        readme.contains("depending on demand"),
        "README.md does not say the Windows addition depends on demand"
    );
}

#[test]
fn readme_documents_model_pinning_from_the_cli() {
    let readme = readme();
    assert!(
        readme.contains("config set implementer"),
        "README.md does not show `config set implementer`"
    );
    assert!(
        readme.contains("provider/model#variant"),
        "README.md does not name the exact-model form provider/model#variant"
    );
    assert!(
        readme
            .lines()
            .any(|line| line.contains("config set") && line.contains('/') && line.contains('#')),
        "README.md has no exact-model example in provider/model#variant form"
    );
    assert!(
        readme.contains("--host"),
        "README.md does not document --host"
    );
    assert!(
        readme.contains("--project"),
        "README.md does not document --project"
    );
}

#[test]
fn the_product_has_one_description() {
    assert_eq!(env!("CARGO_PKG_DESCRIPTION"), DESCRIPTION);
    assert_eq!(Cli::command().get_about().unwrap().to_string(), DESCRIPTION);
    assert!(read(".claude-plugin/marketplace.json").contains(DESCRIPTION));
    assert!(read("adapters/claude-plugin/.claude-plugin/plugin.json").contains(DESCRIPTION));
}

#[test]
fn github_anchor_follows_githubs_rule() {
    assert_eq!(github_anchor("Reading the output"), "reading-the-output");
    assert_eq!(github_anchor("relaxkit config set"), "relaxkit-config-set");
    assert_eq!(github_anchor("What's new?"), "whats-new");
}

#[test]
fn relative_links_resolve() {
    for file in public_markdown() {
        let dir = Path::new(&file).parent().unwrap_or(Path::new(""));
        for (path, anchor) in relative_links(&read(&file)) {
            let target = if path.is_empty() {
                file.clone()
            } else {
                dir.join(&path).to_string_lossy().into_owned()
            };
            assert!(
                exists(&target),
                "{file}: link to `{path}` points at a missing file"
            );
            if let Some(anchor) = anchor.filter(|_| target.ends_with(".md")) {
                let found = unfenced_lines(&read(&target)).iter().any(|line| {
                    line.starts_with('#')
                        && github_anchor(line.trim_start_matches('#').trim()) == anchor
                });
                assert!(
                    found,
                    "{file}: link `{path}#{anchor}` has no matching heading in {target}"
                );
            }
        }
    }
}

#[test]
fn shell_examples_parse() {
    for file in public_markdown() {
        for example in sh_examples(&read(&file)) {
            if !(example.starts_with("relaxkit ") || example.starts_with("rlx ")) {
                continue;
            }
            let words: Vec<&str> = example.split_whitespace().collect();
            let args = std::iter::once("relaxkit").chain(words.iter().skip(1).copied());
            if let Err(err) = Cli::try_parse_from(args) {
                let help = matches!(
                    err.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                );
                assert!(help, "{file}: `{example}` does not parse: {err}");
            }
        }
    }
}

#[test]
fn public_docs_carry_no_personal_data() {
    let mut files = public_markdown();
    if let Ok(entries) = std::fs::read_dir(repo_path(".github/ISSUE_TEMPLATE")) {
        for entry in entries {
            let name = entry.expect("ISSUE_TEMPLATE entry is readable").file_name();
            files.push(format!(".github/ISSUE_TEMPLATE/{}", name.to_string_lossy()));
        }
    }
    for file in files {
        let text = read(&file);
        for banned in [
            "@gmail.com",
            "/Users/",
            "claude.ai/code/session",
            "Claude-Session",
        ] {
            assert!(!text.contains(banned), "{file} contains `{banned}`");
        }
    }
}

#[test]
fn public_docs_cite_no_private_artifacts() {
    const PHRASES: [&str; 7] = [
        "the owner",
        "owner's",
        "spec §",
        "spec section",
        "ruling ",
        "docs/superpowers",
        "docs/spikes",
    ];
    let cited = |text: &str| {
        let lower = text.to_lowercase();
        PHRASES
            .iter()
            .copied()
            .find(|phrase| lower.contains(*phrase))
    };
    for file in public_markdown() {
        if PENDING_PRIVATE_REFERENCES.contains(&file.as_str()) {
            continue;
        }
        if let Some(phrase) = cited(&read(&file)) {
            panic!("{file} cites the private artifact phrase `{phrase}`");
        }
    }
    // A pending file that no longer cites anything must leave PENDING_PRIVATE_REFERENCES.
    for file in PENDING_PRIVATE_REFERENCES {
        assert!(
            cited(&read(file)).is_some(),
            "{file} no longer cites private artifacts: remove it from PENDING_PRIVATE_REFERENCES"
        );
    }
}

/// Splits a `## [x.y.z] - YYYY-MM-DD` heading into its version and date, or `None` when it has another shape.
fn release_heading(line: &str) -> Option<((u64, u64, u64), &str)> {
    let rest = line.strip_prefix("## [")?;
    let (version, date) = rest.split_once("] - ")?;
    let parts: Vec<u64> = version
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let [major, minor, patch] = parts[..] else {
        return None;
    };
    let date_ok = date.len() == 10
        && date.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    date_ok.then_some(((major, minor, patch), date))
}

#[test]
fn changelog_follows_keep_a_changelog() {
    let text = read("CHANGELOG.md");
    let lines = unfenced_lines(&text);
    assert_eq!(lines.first().copied(), Some("# Changelog"));

    let headings: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("## "))
        .map(|(at, line)| (at, *line))
        .collect();
    assert_eq!(
        headings.first().map(|(_, line)| *line),
        Some("## [Unreleased]"),
        "CHANGELOG.md has no `## [Unreleased]` heading before its versions"
    );
    let (unreleased_at, _) = headings[0];
    let unreleased_end = headings.get(1).map_or(lines.len(), |(at, _)| *at);
    for line in &lines[unreleased_at + 1..unreleased_end] {
        assert!(
            line.trim().is_empty(),
            "`## [Unreleased]` must stay empty, found `{line}`"
        );
    }

    let mut versions = Vec::new();
    for (_, line) in &headings[1..] {
        let ((major, minor, patch), _) = release_heading(line)
            .unwrap_or_else(|| panic!("`{line}` does not match `## [x.y.z] - YYYY-MM-DD`"));
        versions.push((major, minor, patch));
    }
    assert!(!versions.is_empty(), "CHANGELOG.md has no release section");
    for pair in versions.windows(2) {
        assert!(pair[0] > pair[1], "release sections must descend: {pair:?}");
    }
    let (major, minor, patch) = versions[0];
    assert_eq!(
        format!("{major}.{minor}.{patch}"),
        env!("CARGO_PKG_VERSION"),
        "the first release section must be the crate version"
    );

    const SECTIONS: [&str; 6] = [
        "Added",
        "Changed",
        "Deprecated",
        "Removed",
        "Fixed",
        "Security",
    ];
    for line in &lines {
        if let Some(section) = line.strip_prefix("### ") {
            assert!(
                SECTIONS.contains(&section.trim()),
                "`### {section}` is not a Keep a Changelog section"
            );
        }
        assert!(
            !(line.starts_with("* ") && line.contains(" by @") && line.contains(" in https://")),
            "`{line}` looks like a pasted release-notes entry"
        );
    }

    let mut names: Vec<String> = vec!["Unreleased".to_string()];
    names.extend(headings[1..].iter().map(|(_, line)| {
        line.trim_start_matches("## [")
            .split(']')
            .next()
            .unwrap_or_default()
            .to_string()
    }));
    for name in names {
        let prefix = format!("[{name}]: https://github.com/cpenaforte/relaxkit/");
        assert!(
            lines.iter().any(|line| line.starts_with(&prefix)),
            "CHANGELOG.md has no link reference for `{name}`"
        );
    }
}
