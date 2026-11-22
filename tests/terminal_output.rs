//! What the text output looks like where a person reads it and where a program reads it: a pipe gets plain text, a
//! terminal without a UTF-8 locale gets ASCII marks, and `doctor --all` is the only way to list the checks that
//! passed.

mod support;

use clap::Parser;
use relaxkit::cli::{Cli, Command};
use support::Scratch;

/// The output of a command that ran with stdout and stderr captured, as text.
fn both(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr)
}

#[test]
fn a_pipe_gets_no_escape_codes_or_carriage_returns() {
    let scratch = Scratch::claude_only();

    let doctor = scratch.relaxkit().arg("doctor").output().unwrap();
    let setup = scratch.relaxkit().arg("setup").output().unwrap();

    for text in [both(&doctor), both(&setup)] {
        assert!(
            !text.contains('\u{1b}'),
            "escape codes in piped output:\n{text}"
        );
        assert!(
            !text.contains('\r'),
            "carriage returns in piped output:\n{text}"
        );
    }
}

#[test]
fn doctor_hides_the_ok_checks_until_all_is_asked_for() {
    let scratch = Scratch::claude_only();

    let plain = scratch.relaxkit().arg("doctor").output().unwrap();
    let plain = String::from_utf8_lossy(&plain.stdout).into_owned();
    assert!(
        plain.contains("ok checks hidden; relaxkit doctor --all lists them"),
        "{plain}"
    );
    assert!(!plain.contains("✓ "), "{plain}");

    let all = scratch
        .relaxkit()
        .args(["doctor", "--all"])
        .output()
        .unwrap();
    let all = String::from_utf8_lossy(&all.stdout).into_owned();
    assert!(all.contains("✓ "), "{all}");
    assert!(!all.contains("hidden"), "{all}");
}

#[test]
fn a_terminal_without_a_utf8_locale_gets_ascii_marks() {
    let scratch = Scratch::claude_only();

    let output = scratch
        .relaxkit()
        .env("LC_ALL", "C")
        .args(["doctor", "--all"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();

    assert!(text.lines().any(|line| line.starts_with("  ok ")), "{text}");
    assert!(!text.contains('✓'), "{text}");
}

#[test]
fn doctor_all_parses_as_a_flag_of_doctor() {
    let cli = Cli::try_parse_from(["relaxkit", "doctor", "--all"]).expect("doctor --all parses");

    assert!(
        matches!(cli.command, Command::Doctor { all: true }),
        "{:?}",
        cli.command
    );
}
