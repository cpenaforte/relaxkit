//! The end-to-end run for the `e2e-linux` CI job (plan Task 28, step 1): the real binary, the Claude Code stub on the PATH,
//! and no prompt. The OpenCode side is not in CI: its stub writes no plugin config, and caveman's OpenCode step reads the
//! network, so both are left to the owner's live run (docs/acceptance.md). The flow is a dry run, then setup, then the doctor report. Each step must exit 0. stdin is closed, so a
//! prompt would fail the step instead of waiting for an answer.

mod support;

use support::{Scratch, write_lock_except};

#[test]
fn dry_run_then_setup_then_doctor_json_exit_zero_without_a_prompt() {
    let scratch = Scratch::claude_only();
    // The lock pins every recipe, so setup needs no GitHub lookup for a latest tag (the network is closed in the test).
    write_lock_except(&scratch, None);

    scratch
        .relaxkit()
        .args(["--dry-run", "setup"])
        .assert()
        .success();
    scratch.relaxkit().arg("setup").assert().success();
    let output = scratch
        .relaxkit()
        .args(["--json", "doctor"])
        .assert()
        .success();

    let report: serde_json::Value =
        serde_json::from_slice(&output.get_output().stdout).expect("doctor --json prints JSON");
    assert!(report["checks"].is_array(), "{report}");
}
