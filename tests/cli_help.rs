mod support;

use assert_cmd::Command;
use predicates::prelude::*;

const SUBCOMMANDS: &[&str] = &[
    "setup",
    "status",
    "doctor",
    "update",
    "config",
    "preset",
    "privacy",
    "login",
    "undo",
    "uninstall",
    "usage",
];

#[test]
fn every_subcommand_has_help() {
    for cmd in SUBCOMMANDS {
        Command::cargo_bin("relaxkit")
            .unwrap()
            .args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage:"));
    }
}

#[test]
fn version_flag_prints_crate_version() {
    Command::cargo_bin("relaxkit")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_subcommand_fails() {
    Command::cargo_bin("relaxkit")
        .unwrap()
        .arg("no-such-command")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn update_self_conflicts_with_a_tool_name() {
    Command::cargo_bin("relaxkit")
        .unwrap()
        .args(["update", "--self", "some-tool"])
        .assert()
        .code(2);
}

#[test]
fn update_tools_conflicts_with_a_tool_name() {
    Command::cargo_bin("relaxkit")
        .unwrap()
        .args(["update", "--tools", "some-tool"])
        .assert()
        .code(2);
}

#[test]
fn rlx_symlink_runs_the_same_binary() {
    let bin = env!("CARGO_BIN_EXE_relaxkit");
    let link = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rlx");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(bin, &link).expect("create rlx symlink");

    for flag in ["--version", "--help"] {
        let direct = std::process::Command::new(bin).arg(flag).output().unwrap();
        let via_link = std::process::Command::new(&link)
            .arg(flag)
            .output()
            .unwrap();
        assert!(direct.status.success(), "{flag} failed on the binary");
        assert!(via_link.status.success(), "{flag} failed through rlx");
        assert_eq!(
            String::from_utf8_lossy(&direct.stdout),
            String::from_utf8_lossy(&via_link.stdout),
            "{flag} output differs"
        );
    }
    let version = std::process::Command::new(&link)
        .arg("--version")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn config_show_parses_to_the_config_variant() {
    use clap::Parser;
    use relaxkit::cli::{Cli, Command, ConfigCmd};

    let cli = Cli::try_parse_from(["relaxkit", "config", "show"]).expect("config show parses");
    assert!(matches!(
        cli.command,
        Command::Config(ConfigCmd::Show { key: None })
    ));
}

#[test]
fn usage_help_flag_before_the_tool_is_relaxkit_help() {
    Command::cargo_bin("relaxkit")
        .unwrap()
        .args(["usage", "--help", "cargo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: relaxkit"));
    Command::cargo_bin("relaxkit")
        .unwrap()
        .args(["usage", "-h", "cargo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: relaxkit"));
}

#[test]
fn usage_help_flag_after_the_tool_reaches_the_handler() {
    // The handler runs the pinned ccusage, so this runs in a Scratch home with the Node link in place: no real
    // home and no real host command. The flag after the tool name is passed through, not read as relaxkit's help.
    let scratch = support::Scratch::new();
    let data = scratch.home().join(".local/share/relaxkit");
    std::fs::create_dir_all(&data).unwrap();
    std::os::unix::fs::symlink(
        scratch.home().join(".nvm/versions/node/v24.13.0"),
        data.join("node"),
    )
    .unwrap();
    // Usage runs only a locked ccusage, so the lock names it first.
    support::write_lock_except(&scratch, None);
    for flag in ["--help", "-h"] {
        scratch.clear_calls();
        scratch
            .relaxkit()
            .args(["usage", "cargo", flag])
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage: relaxkit").not());
        assert!(
            scratch
                .calls()
                .iter()
                .any(|call| call.contains("ccusage@") && call.contains(&format!("cargo {flag}"))),
            "{flag} must reach ccusage: {:?}",
            scratch.calls()
        );
    }
}

/// Each example line under the `Examples:` block of a help text, without the indent.
fn help_example_lines(help: &str) -> Vec<String> {
    let mut in_examples = false;
    let mut examples = Vec::new();
    for line in help.lines() {
        if line == "Examples:" {
            in_examples = true;
        } else if in_examples && line.starts_with("  relaxkit ") {
            examples.push(line.trim_start().to_string());
        } else if in_examples && line.trim().is_empty() {
            in_examples = false;
        }
    }
    examples
}

fn help_text(args: &[&str]) -> String {
    let output = Command::cargo_bin("relaxkit")
        .unwrap()
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?} --help failed");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn root_help_has_examples_and_the_docs_link() {
    let help = help_text(&["--help"]);
    assert!(
        help.contains("Examples:"),
        "root help has no examples:\n{help}"
    );
    let link = format!(
        "https://github.com/{}/tree/main/docs",
        relaxkit::brand::REPO
    );
    let last = help.lines().rfind(|line| !line.trim().is_empty()).unwrap();
    assert_eq!(last, format!("Documentation: {link}"));
}

#[test]
fn setup_config_and_update_help_have_examples() {
    for cmd in ["setup", "config", "update"] {
        let help = help_text(&[cmd, "--help"]);
        assert!(
            help.contains("Examples:"),
            "{cmd} help has no examples:\n{help}"
        );
    }
}

#[test]
fn help_examples_parse() {
    use clap::Parser;
    use relaxkit::cli::Cli;

    for args in [
        &["--help"][..],
        &["setup", "--help"],
        &["config", "--help"],
        &["update", "--help"],
    ] {
        let help = help_text(args);
        let examples = help_example_lines(&help);
        assert!(!examples.is_empty(), "{args:?} help has no example lines");
        for example in examples {
            let words = example.split_whitespace();
            Cli::try_parse_from(words)
                .unwrap_or_else(|err| panic!("{args:?} example `{example}` does not parse: {err}"));
        }
    }
}
