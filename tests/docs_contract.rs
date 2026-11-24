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

fn configuration_reference() -> String {
    read("docs/configuration.md")
}

fn tools_reference() -> String {
    read("docs/tools.md")
}

fn privacy_reference() -> String {
    read("docs/privacy.md")
}

fn sandbox_reference() -> String {
    read("docs/sandbox.md")
}

#[test]
fn privacy_lists_every_telemetry_switch() {
    let doc = privacy_reference();
    for form in [
        "rtk telemetry disable",
        "caveman telemetry off",
        "SUPERPOWERS_DISABLE_TELEMETRY",
        "DISABLE_TELEMETRY",
        "relaxkit privacy",
    ] {
        assert!(
            doc.contains(form),
            "docs/privacy.md does not mention `{form}`"
        );
    }
}

#[test]
fn sandbox_doc_names_the_commands_that_run_outside_the_sandbox() {
    let doc = sandbox_reference();
    for command in relaxkit::sandbox::claude::PACK_COMMANDS {
        let form = format!("`{command}`");
        assert!(
            doc.contains(&form),
            "docs/sandbox.md does not name the pack command {form}"
        );
    }
    for form in ["sandbox.excludedCommands", "`gh`"] {
        assert!(
            doc.contains(form),
            "docs/sandbox.md does not mention `{form}`"
        );
    }
}

#[test]
fn configuration_documents_model_pinning() {
    let doc = configuration_reference();
    for form in [
        "config set implementer",
        "provider/model#variant",
        "--host",
        "--project",
    ] {
        assert!(
            doc.contains(form),
            "docs/configuration.md does not show `{form}`"
        );
    }
    assert!(
        doc.lines()
            .any(|line| line.contains("config set") && line.contains('/') && line.contains('#')),
        "docs/configuration.md has no exact-model example in provider/model#variant form"
    );
}

#[test]
fn configuration_lists_every_role_tier_and_preset() {
    let doc = configuration_reference();
    for word in [
        "planner",
        "implementer",
        "reviewer",
        "escalation",
        "final-review",
        "cheap",
        "standard",
        "smarter",
        "lean",
        "balanced",
        "careful",
        "voice",
        "compact-at",
    ] {
        assert!(
            doc.contains(word),
            "docs/configuration.md does not mention `{word}`"
        );
    }
}

#[test]
fn tools_reference_lists_every_recipe() {
    let doc = tools_reference();
    let entries = std::fs::read_dir(repo_path("recipes")).expect("recipes/ is readable");
    let mut count = 0;
    for entry in entries {
        let path = entry.expect("recipes/ entry is readable").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        count += 1;
        let text = std::fs::read_to_string(&path).expect("recipe is readable");
        let id = text
            .lines()
            .find_map(|line| {
                let rest = line.strip_prefix("id = \"")?;
                rest.strip_suffix('"')
            })
            .unwrap_or_else(|| panic!("{} has no `id = \"...\"` line", path.display()));
        let code = format!("`{id}`");
        assert!(
            doc.contains(&code),
            "docs/tools.md does not list the recipe `{id}`"
        );
    }
    assert!(count > 0, "recipes/ has no recipe files");
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
fn readme_is_a_front_door() {
    let readme = readme();
    let lines = unfenced_lines(&readme);
    let headings = [
        "# relaxkit",
        "## Highlights",
        "## Install",
        "## Quickstart",
        "## Documentation",
        "## Getting help",
        "## Contributing",
        "## License",
    ];
    let mut from = 0;
    for heading in headings {
        let at = lines[from..]
            .iter()
            .position(|line| *line == heading)
            .map(|offset| from + offset)
            .unwrap_or_else(|| {
                panic!("README.md has no heading `{heading}` after the one before it")
            });
        from = at + 1;
    }
    assert!(
        readme.contains(DESCRIPTION),
        "README.md does not contain the product description"
    );
}

#[test]
fn readme_links_the_documents() {
    let readme = readme();
    let mut required = vec![
        "](docs/getting-started.md)",
        "](docs/commands.md)",
        "](docs/configuration.md)",
        "](docs/tools.md)",
        "](docs/privacy.md)",
        "](docs/sandbox.md)",
        "](docs/troubleshooting.md)",
        "](docs/verification.md)",
        "](CONTRIBUTING.md)",
        "](SECURITY.md)",
        "](CHANGELOG.md)",
        "](LICENSE-MIT)",
        "](LICENSE-APACHE)",
    ];
    if exists("CODE_OF_CONDUCT.md") {
        required.push("](CODE_OF_CONDUCT.md)");
    }
    for link in required {
        assert!(readme.contains(link), "README.md does not contain `{link}`");
    }
    assert_eq!(
        readme.contains("](CODE_OF_CONDUCT.md)"),
        exists("CODE_OF_CONDUCT.md"),
        "README.md links CODE_OF_CONDUCT.md if and only if the file exists"
    );
}

/// The URL of every image link (`![alt](url)`) in `markdown`.
fn image_urls(markdown: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for line in unfenced_lines(markdown) {
        let mut rest = line;
        while let Some(at) = rest.find("![") {
            let after = &rest[at..];
            let Some(open) = after.find("](") else {
                break;
            };
            let target = &after[open + 2..];
            let Some(close) = target.find(')') else {
                break;
            };
            urls.push(target[..close].to_string());
            rest = &target[close + 1..];
        }
    }
    urls
}

#[test]
fn readme_badges_point_at_this_repository() {
    let badges: Vec<String> = image_urls(&readme())
        .into_iter()
        .filter(|url| url.contains("badge") || url.contains("shields.io"))
        .collect();
    assert!(
        (1..=4).contains(&badges.len()),
        "README.md has {} badges, expected 1 to 4",
        badges.len()
    );
    for badge in badges {
        assert!(
            badge.contains("cpenaforte/relaxkit"),
            "badge `{badge}` does not point at this repository"
        );
    }
}

#[test]
fn readme_makes_no_claim_about_unpublished_channels() {
    let readme = readme();
    for form in ["brew install", "cargo install relaxkit"] {
        assert!(
            !readme.contains(form),
            "README.md mentions `{form}`, which is not published yet"
        );
    }
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
        if let Some(phrase) = cited(&read(&file)) {
            panic!("{file} cites the private artifact phrase `{phrase}`");
        }
    }
}

#[test]
fn docs_index_links_every_document() {
    let index = read("docs/README.md");
    let mut documents: Vec<String> = std::fs::read_dir(repo_path("docs"))
        .expect("docs/ is readable")
        .map(|entry| {
            let name = entry.expect("docs/ entry is readable").file_name();
            name.to_string_lossy().into_owned()
        })
        .filter(|name| name.ends_with(".md") && name != "README.md")
        .collect();
    documents.sort();
    assert!(!documents.is_empty(), "docs/ holds no documents to index");
    for name in &documents {
        let link = format!("]({name})");
        assert!(
            index.contains(&link),
            "docs/README.md does not link to {name} as `{link}`"
        );
    }
    let changelog = "../CHANGELOG.md";
    assert!(
        index.contains(&format!("]({changelog})")),
        "docs/README.md does not link to the changelog as `]({changelog})`"
    );
    let expected: std::collections::BTreeSet<String> = documents
        .into_iter()
        .chain(std::iter::once(changelog.to_string()))
        .collect();
    let linked: std::collections::BTreeSet<String> = relative_links(&index)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        linked, expected,
        "docs/README.md must link exactly the documents and the changelog"
    );
    for path in &linked {
        assert!(
            repo_path("docs").join(path).is_file(),
            "docs/README.md links to {path}, which does not exist"
        );
    }
}

/// The public files that are not Markdown: the recipes, the scripts, the Homebrew templates and the GitHub workflows.
fn non_markdown_public_files() -> Vec<String> {
    let mut files = Vec::new();
    for (dir, ext) in [
        ("recipes", "toml"),
        ("scripts", "sh"),
        ("packaging/homebrew", "tmpl"),
        (".github/workflows", "yml"),
    ] {
        let entries = std::fs::read_dir(repo_path(dir))
            .unwrap_or_else(|err| panic!("{dir}/ is readable: {err}"));
        for entry in entries {
            let entry = entry.expect("directory entry is readable");
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some(ext) {
                files.push(format!("{dir}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no non-Markdown public files found");
    files
}

#[test]
fn non_markdown_public_files_cite_no_private_artifacts() {
    const PHRASES: [&str; 6] = [
        "spec §",
        "spec commit",
        "ruling ",
        "the owner",
        "owner decision",
        "spec section",
    ];
    // A task or spec word followed directly by a number cites a numbered item of a private file.
    let numbered = |lower: &str, word: &str| {
        lower
            .match_indices(word)
            .any(|(at, _)| lower[at + word.len()..].starts_with(|c: char| c.is_ascii_digit()))
    };
    let cited = |text: &str| {
        let lower = text.to_lowercase();
        if let Some(phrase) = PHRASES.iter().find(|phrase| lower.contains(*phrase)) {
            return Some(phrase.to_string());
        }
        ["task ", "spec "]
            .into_iter()
            .find(|word| numbered(&lower, word))
            .map(|word| format!("{word}<number>"))
    };
    for file in non_markdown_public_files() {
        if let Some(phrase) = cited(&read(&file)) {
            panic!("{file} cites the private artifact phrase `{phrase}`");
        }
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

/// The Keep a Changelog section headings that an entry may sit under.
const SECTIONS: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// The first line under `## [Unreleased]` that breaks the rule, or `None`. A `### ` heading must name a section. A
/// bullet, and an indented line that directly follows a bullet or another such line (a wrapped bullet), must sit under
/// one.
fn unreleased_problem(body: &[&str]) -> Option<String> {
    let mut in_section = false;
    // True right after a bullet or a wrapped line, so an indented line may continue it.
    let mut in_entry = false;
    for line in body {
        if line.trim().is_empty() {
            in_entry = false;
            continue;
        }
        if let Some(name) = line.strip_prefix("### ") {
            if !SECTIONS.contains(&name.trim()) {
                return Some(format!(
                    "has the heading `{line}`, which is not a Keep a Changelog section"
                ));
            }
            in_section = true;
            in_entry = false;
        } else if line.starts_with("- ") {
            if !in_section {
                return Some(format!(
                    "has the bullet `{line}` outside a Keep a Changelog section"
                ));
            }
            in_entry = true;
        } else if line.starts_with([' ', '\t']) && is_atx_heading(line.trim_start()) {
            return Some(format!(
                "has the indented heading `{line}`, which is not allowed under `## [Unreleased]`"
            ));
        } else if in_entry && line.starts_with([' ', '\t']) {
            if !in_section {
                return Some(format!(
                    "has the wrapped line `{line}` outside a Keep a Changelog section"
                ));
            }
        } else {
            return Some(format!(
                "has the line `{line}`, which is neither a section heading nor a bullet"
            ));
        }
    }
    None
}

/// True when `text` is an ATX heading: one to six `#` followed by a space or the end of the line.
fn is_atx_heading(text: &str) -> bool {
    let rest = text.trim_start_matches('#');
    let hashes = text.len() - rest.len();
    (1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with(' '))
}

#[test]
fn unreleased_bullet_outside_a_section_fails() {
    assert!(
        unreleased_problem(&["", "- Fixed a thing."]).is_some(),
        "a bullet with no section heading above it must fail"
    );
    assert_eq!(
        unreleased_problem(&["", "### Fixed", "", "- Fixed a thing."]),
        None,
        "a bullet under a section heading must pass"
    );
}

#[test]
fn unreleased_wrapped_bullet_follows_the_section_rule() {
    assert_eq!(
        unreleased_problem(&[
            "### Fixed",
            "",
            "- Fixed a thing that",
            "  wraps onto a second line."
        ]),
        None,
        "a wrapped bullet under a section heading must pass"
    );
    assert_eq!(
        unreleased_problem(&["### Fixed", "- one", "  two", "    three"]),
        None,
        "a continuation of a continuation under a section must pass"
    );
    assert!(
        unreleased_problem(&["- Fixed a thing that", "  wraps onto a second line."]).is_some(),
        "a wrapped bullet with no section heading above it must fail"
    );
    assert!(
        unreleased_problem(&["### Fixed", "", "  indented with no bullet above it"]).is_some(),
        "an indented line that does not follow a bullet must fail"
    );
    assert!(
        unreleased_problem(&["### Fixed", "- a", "", "  b"]).is_some(),
        "a continuation after a blank line must fail"
    );
}

#[test]
fn unreleased_rejects_an_indented_heading() {
    assert!(
        unreleased_problem(&["### Fixed", "- a", "  ### Bogus"]).is_some(),
        "an indented heading under `## [Unreleased]` must fail"
    );
    assert!(
        unreleased_problem(&["### Fixed", "- a", "\t### Bogus"]).is_some(),
        "a tab-indented heading must fail the same way"
    );
}

#[test]
fn unreleased_accepts_a_wrapped_line_that_starts_with_a_hash() {
    assert_eq!(
        unreleased_problem(&["### Fixed", "- a", "  #42 was fixed"]),
        None,
        "an indented `#42` is a wrapped line, not an ATX heading"
    );
    assert_eq!(
        unreleased_problem(&["### Fixed", "- a", "\t#42 was fixed"]),
        None,
        "a tab-indented `#42` is a wrapped line too"
    );
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
    if let Some(problem) = unreleased_problem(&lines[unreleased_at + 1..unreleased_end]) {
        panic!("`## [Unreleased]` {problem}");
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

fn getting_started() -> String {
    read("docs/getting-started.md")
}

fn troubleshooting() -> String {
    read("docs/troubleshooting.md")
}

#[test]
fn getting_started_walks_the_first_run_in_order() {
    let doc = getting_started();
    let steps = [
        "relaxkit --version",
        "relaxkit setup",
        "relaxkit status",
        "relaxkit doctor",
        "relaxkit config set implementer cheap",
        "relaxkit undo",
    ];
    let mut from = 0;
    for step in steps {
        let at = doc[from..]
            .find(step)
            .map(|offset| from + offset)
            .unwrap_or_else(|| {
                panic!("docs/getting-started.md has no `{step}` after the step before it")
            });
        from = at + 1;
    }
}

#[test]
fn troubleshooting_names_each_known_failure() {
    let doc = troubleshooting();
    for form in [
        "/login",
        "bubblewrap",
        "socat",
        "relaxkit login github",
        "no known executable path",
        "rlx",
    ] {
        assert!(
            doc.contains(form),
            "docs/troubleshooting.md does not name `{form}`"
        );
    }
}

fn verification() -> String {
    read("docs/verification.md")
}

/// The table rows of `doc` as cells, with the header and the separator row left out.
fn table_rows(doc: &str) -> Vec<Vec<String>> {
    unfenced_lines(doc)
        .into_iter()
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|cells| {
            let separator = cells
                .iter()
                .all(|cell| cell.chars().all(|c| c == '-' || c == ':'));
            !separator && cells[0] != "#"
        })
        .collect()
}

#[test]
fn verification_rows_use_the_result_vocabulary_and_cite_evidence() {
    let doc = verification();
    assert!(
        unfenced_lines(&doc)
            .contains(&"| # | Criterion | How to check | Result | Date | Evidence |"),
        "docs/verification.md has no table with the columns # | Criterion | How to check | Result | Date | Evidence"
    );
    let rows = table_rows(&doc);
    assert!(!rows.is_empty(), "docs/verification.md has no result rows");
    for row in rows {
        assert_eq!(row.len(), 6, "row `{}` does not have six cells", row[0]);
        let result = row[3].as_str();
        assert!(
            matches!(result, "pass" | "fail" | "not verified"),
            "row `{}` has the Result `{result}`, which is not pass, fail or not verified",
            row[0]
        );
        if result == "pass" {
            let date = row[4].as_str();
            let date_ok = date.len() == 10
                && date.char_indices().all(|(i, c)| match i {
                    4 | 7 => c == '-',
                    _ => c.is_ascii_digit(),
                });
            assert!(date_ok, "pass row `{}` has no YYYY-MM-DD date", row[0]);
            assert!(!row[5].is_empty(), "pass row `{}` has no Evidence", row[0]);
        }
    }
}

#[test]
fn sandbox_doc_dates_the_opencode_live_check_and_links_the_verification_record() {
    let doc = sandbox_reference();
    assert!(
        doc.contains("checked live against OpenCode 2.2.6 on 2026-11-22"),
        "docs/sandbox.md does not date the OpenCode live check"
    );
    assert!(
        doc.contains("](verification.md)"),
        "docs/sandbox.md does not link to docs/verification.md"
    );
}

/// The three commands CI runs, in the order of `.github/workflows/ci.yml`.
const CI_COMMANDS: [&str; 3] = [
    "cargo fmt --check",
    "cargo clippy --all-targets -- -D warnings",
    "cargo test --no-fail-fast",
];

/// The lines of `doc` under `heading`, up to the next `## ` heading. Fenced code is kept.
fn section(doc: &str, heading: &str) -> String {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in doc.lines() {
        if line == heading {
            inside = true;
            continue;
        }
        if inside && line.starts_with("## ") {
            break;
        }
        if inside {
            lines.push(line);
        }
    }
    assert!(inside, "no heading `{heading}`");
    lines.join("\n")
}

#[test]
fn contributing_commands_match_ci() {
    let contributing = read("CONTRIBUTING.md");
    let ci = read(".github/workflows/ci.yml");
    for command in CI_COMMANDS {
        assert!(
            contributing.contains(command),
            "CONTRIBUTING.md does not name `{command}`"
        );
        assert!(
            ci.contains(command),
            ".github/workflows/ci.yml does not run `{command}`"
        );
    }
}

#[test]
fn release_checklist_files_agree_on_the_version() {
    let version = env!("CARGO_PKG_VERSION");
    for file in [
        "adapters/claude-plugin/.claude-plugin/plugin.json",
        "adapters/opencode-plugin/package.json",
    ] {
        let json: serde_json::Value = serde_json::from_str(&read(file))
            .unwrap_or_else(|err| panic!("{file} is not JSON: {err}"));
        assert_eq!(
            json["version"].as_str(),
            Some(version),
            "{file} does not carry the Cargo.toml version {version}"
        );
    }
    let releasing = section(&read("CONTRIBUTING.md"), "## Releasing");
    for file in [
        "Cargo.toml",
        "Cargo.lock",
        "adapters/claude-plugin/.claude-plugin/plugin.json",
        "adapters/opencode-plugin/package.json",
        "adapters/opencode-plugin/package-lock.json",
    ] {
        assert!(
            releasing.contains(file),
            "CONTRIBUTING.md `## Releasing` does not name `{file}`"
        );
    }
}

#[test]
fn security_policy_has_a_private_channel_and_a_scope() {
    let doc = read("SECURITY.md");
    for heading in [
        "## Supported versions",
        "## Reporting a vulnerability",
        "## Scope",
        "## Verifying a release",
    ] {
        assert!(
            doc.lines().any(|line| line == heading),
            "SECURITY.md has no heading `{heading}`"
        );
    }
    for form in [
        "## Supported versions",
        "security/advisories/new",
        "14 days",
        "minisign",
        "release/minisign.pub",
    ] {
        assert!(doc.contains(form), "SECURITY.md does not contain `{form}`");
    }
}

#[test]
fn issue_forms_have_the_keys_github_requires() {
    let mut forms = 0;
    let entries = std::fs::read_dir(repo_path(".github/ISSUE_TEMPLATE"))
        .expect(".github/ISSUE_TEMPLATE is readable");
    for entry in entries {
        let name = entry
            .expect(".github/ISSUE_TEMPLATE entry is readable")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name == "config.yml" || !name.ends_with(".yml") {
            continue;
        }
        forms += 1;
        let text = read(&format!(".github/ISSUE_TEMPLATE/{name}"));
        for key in ["name:", "description:", "body:"] {
            assert!(
                text.lines().any(|line| line.starts_with(key)),
                ".github/ISSUE_TEMPLATE/{name} has no top-level `{key}` key"
            );
        }
    }
    assert!(forms > 0, ".github/ISSUE_TEMPLATE has no issue forms");

    let config = read(".github/ISSUE_TEMPLATE/config.yml");
    assert!(
        config
            .lines()
            .any(|line| line == "blank_issues_enabled: false"),
        "config.yml does not set `blank_issues_enabled: false`"
    );
    assert!(
        config.contains("security/advisories/new"),
        "config.yml has no contact link to private security reporting"
    );
}

#[test]
fn pull_request_template_asks_for_the_checks_and_a_changelog_entry() {
    let doc = read(".github/pull_request_template.md");
    for command in CI_COMMANDS {
        assert!(
            doc.contains(command),
            ".github/pull_request_template.md does not ask for `{command}`"
        );
    }
    for word in ["CHANGELOG", "docs"] {
        assert!(
            doc.contains(word),
            ".github/pull_request_template.md does not mention `{word}`"
        );
    }
}

#[test]
fn agents_file_matches_ci_and_stays_short() {
    let doc = read("AGENTS.md");
    assert!(
        doc.contains(DESCRIPTION),
        "AGENTS.md does not state the product description"
    );
    for command in CI_COMMANDS {
        assert!(doc.contains(command), "AGENTS.md does not name `{command}`");
    }
    let lines = doc.lines().count();
    assert!(lines <= 80, "AGENTS.md has {lines} lines, more than 80");
}
