# Acceptance

The spec's success criteria, with the result of each check. Results are recorded by the owner's live run on a macOS scratch home with the real `claude` and `opencode` binaries. The Linux row is recorded by the `e2e-linux` CI job.

Status: every row is pending until the owner's run. Nothing here has been run against the real hosts.

| # | Criterion | Check | Result | Date | Notes |
| --- | --- | --- | --- | --- | --- |
| 1 | `relaxkit setup` leaves both hosts set up and asks only for the GitHub login | `relaxkit setup` in a scratch home | pending | | |
| 2 | `relaxkit update --check` and `relaxkit update` succeed | run both | pending | | |
| 3 | A pinned role shows in the config | `relaxkit config set implementer anthropic/claude-haiku-5-5`, then `relaxkit config show` | pending | | |
| 4 | `relaxkit doctor` passes, including the sandbox self-test | `relaxkit doctor` | pending | | |
| 5 | A subagent-driven run on OpenCode sends the implementer and reviewer prompts to the right agents (spec §10, item 21) | a live OpenCode session | pending | | |
| 6 | `relaxkit undo` and `relaxkit uninstall` restore the scratch home | run both in the scratch home | pending | | |
| CI | Linux: dry run, setup and `doctor --json` exit 0 with no prompt | `cargo test --test e2e_linux` in the `e2e-linux` job, `ubuntu-24.04` | pending: first run after the push | | |

## Not yet checked live

- The `/relaxkit:<subcommand>` slash commands in Claude Code. The README says so until the owner confirms it.
- The OpenCode slash commands. The README's Sandbox section says they are not checked live yet.
- The OpenCode sandbox plugin on OpenCode 2.2.6 was checked live on 2026-11-22 (README, Sandbox). Row 4 covers it again in the full run.
