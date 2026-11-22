---
description: Revert the last change, or the named run group with --run
argument-hint: "[--run GROUP]"
allowed-tools: Bash(relaxkit:*)
---

Revert the last change, or the run group named with `--run`.

!`relaxkit undo $ARGUMENTS`

Show the output exactly as printed.

This command runs outside the sandbox because it changes the machine (`sandbox.excludedCommands`).
