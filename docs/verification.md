# Verification

The checks behind the pack's success criteria, with the result of each one and where its evidence lives. A result is `pass`, `fail` or `not verified`. A `pass` row gives the date of the check and the evidence for it. A `not verified` row has no live result yet, and its Evidence cell says what is needed.

No check was run for this page on a real Claude Code or OpenCode install. Where a row cites a test, the test runs with fixtures and does not touch a real host.

| # | Criterion | How to check | Result | Date | Evidence |
| --- | --- | --- | --- | --- | --- |
| 1 | `relaxkit setup` leaves both hosts set up and asks only for the GitHub login | `relaxkit setup` in a disposable HOME | not verified | | Needs a logged-in Claude Code and a logged-in OpenCode. Fixture tests in `tests/setup.rs` cover setup without the real hosts. |
| 2 | `relaxkit update --check` and `relaxkit update` succeed | `relaxkit update --check`, then `relaxkit update`, in a disposable HOME | not verified | | Not re-run yet. Fixture tests in `tests/update.rs` cover the update path. |
| 3 | A pinned role shows in the config | `relaxkit config set implementer anthropic/claude-haiku-5-5`, then `relaxkit config show`, in a disposable HOME | not verified | | Not re-run yet. Fixture tests in `tests/config_cmd.rs` cover `config set` and `config show`. |
| 4 | `relaxkit doctor` passes, including the sandbox self-test | `relaxkit doctor` on a machine with both hosts installed and logged in | not verified | | Needs both hosts installed and logged in. Fixture tests in `tests/doctor.rs` cover the checks. |
| 5 | A subagent-driven run on OpenCode sends the implementer and reviewer prompts to the right agents | A live OpenCode session with the pack's roles pinned | not verified | | Needs a live OpenCode session. The OpenCode side is not in CI. |
| 6 | `relaxkit undo` and `relaxkit uninstall` restore the scratch home | `relaxkit undo`, then `relaxkit uninstall`, in a disposable HOME | not verified | | Not re-run yet. Fixture tests in `tests/config_cmd.rs` cover `undo` and `uninstall`. |
| CI | Linux: dry run, setup and `doctor --json` exit 0 with no prompt | `cargo test --test e2e_linux` in the `e2e-linux` job, `ubuntu-24.04` | pass | 2026-11-23 | Run [45281095711](https://github.com/cpenaforte/relaxkit/actions/runs/45281095711), the latest green `e2e-linux` run, on tag `v0.1.3` at commit `e7e479b`. Found with `gh run list --workflow e2e-linux.yml --limit 3`. The `docs/alignment` branch has not run this job yet. |
| 7 | The `/relaxkit:<subcommand>` slash commands work in Claude Code | Run one `/relaxkit:<subcommand>` command in a live Claude Code session | not verified | | Needs a live Claude Code session with the relaxkit plugin installed. The README says this form is not yet confirmed. |
| 8 | The OpenCode slash commands work in OpenCode | Run one slash command in a live OpenCode session | not verified | | Needs a live OpenCode session. The README's Sandbox section says these are not checked live yet. |

## Recorded in the 0.1.3 README, not re-run

The README at the `v0.1.3` tag says that the sandbox was checked live against OpenCode 2.2.6 on 2026-11-22. This page quotes that statement from the README (`git show v0.1.3:README.md`, the OpenCode part of the Sandbox section). It is recorded in the 0.1.3 README, not re-run for this page.
