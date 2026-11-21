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
        .failure();
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
