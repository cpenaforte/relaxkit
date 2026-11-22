---
description: Update relaxkit itself, then each tool, and update the lockfile
argument-hint: "[--self | --tools] [--check] [TOOL]"
allowed-tools: Bash(relaxkit:*)
---

Update relaxkit itself, then each tool, and update the lockfile.

!`relaxkit update $ARGUMENTS`

Show the output exactly as printed.

This command runs outside the sandbox because it changes the machine (`sandbox.excludedCommands`).
