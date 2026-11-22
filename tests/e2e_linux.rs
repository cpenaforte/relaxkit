//! The end-to-end run for the `e2e-linux` CI job (plan Task 28, step 1): the real binary, the Claude Code stub on the PATH,
//! and no prompt. The flow is a dry run, then setup, then the doctor report. Each step must exit 0. stdin is closed, so a
//! prompt would fail the step instead of waiting for an answer.
//!
//! The run does not depend on what the host has installed. On Linux the sandbox needs bubblewrap and socat, and on Ubuntu
//! 24.04 and later an AppArmor profile: the stub bin holds `bwrap` and `socat`, and `RELAXKIT_APPARMOR_PROFILE` points at
//! a file in the scratch home. The prerequisite rules themselves are unit-tested in `src/platform.rs`. The OpenCode side is
//! not in CI: its stub writes no plugin config, and caveman's OpenCode step reads the network, so the owner's live run covers
//! it (docs/acceptance.md).

mod support;

use std::fs;

use support::{Scratch, write_lock_except};

#[test]
fn dry_run_then_setup_then_doctor_json_exit_zero_without_a_prompt() {
    let scratch = Scratch::claude_only();
    // The lock pins every recipe, so setup needs no GitHub lookup for a latest tag (the network is closed in the test).
    write_lock_except(&scratch, None);
    let profile = scratch.home().join("relaxkit-bwrap");
    fs::write(&profile, "").unwrap();

    scratch
        .relaxkit()
        .env("RELAXKIT_APPARMOR_PROFILE", &profile)
        .args(["--dry-run", "setup"])
        .assert()
        .success();
    scratch
        .relaxkit()
        .env("RELAXKIT_APPARMOR_PROFILE", &profile)
        .arg("setup")
        .assert()
        .success();
    let output = scratch
        .relaxkit()
        .env("RELAXKIT_APPARMOR_PROFILE", &profile)
        .args(["--json", "doctor"])
        .assert()
        .success();

    let report: serde_json::Value =
        serde_json::from_slice(&output.get_output().stdout).expect("doctor --json prints JSON");
    assert!(report["checks"].is_array(), "{report}");
}
