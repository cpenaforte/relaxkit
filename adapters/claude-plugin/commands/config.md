---
description: Show, set or unset roles, tiers and other preferences
argument-hint: "set KEY VALUE [--host claude|opencode] | unset KEY [--host claude|opencode] | show [KEY]"
allowed-tools: Bash(relaxkit:*)
---

Show, set or unset role tiers, exact models and other preferences.

!`relaxkit config $ARGUMENTS`

Show the output exactly as printed.

This command runs outside the sandbox because it changes the machine (`sandbox.excludedCommands`).
