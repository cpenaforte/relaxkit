//! README checks (Task 27, part B, and Task 24, part B for the slash-command section).
//!
//! The subcommand list comes from the real `Cli`, so a subcommand added to the CLI and missing from the README fails
//! here.

use clap::CommandFactory;
use relaxkit::cli::Cli;

fn readme() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md is readable")
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

#[test]
fn readme_documents_the_slash_commands() {
    let readme = readme();
    for form in [
        "/relaxkit:config set implementer cheap",
        "#high",
        "--host",
        "--project",
    ] {
        assert!(readme.contains(form), "README.md does not show `{form}`");
    }
    let names: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|cmd| cmd.get_name().to_string())
        .filter(|name| name != "help")
        .collect();
    assert!(!names.is_empty(), "the Cli has no subcommands");
    for name in names {
        let form = format!("/relaxkit:{name}");
        assert!(
            readme.contains(&form),
            "README.md does not mention the slash command `{form}`"
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
