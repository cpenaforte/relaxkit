---
description: Remove the pack's MCP entries, config keys, roles blocks and binary copies
allowed-tools: Bash(relaxkit:*)
---

Remove the pack's MCP entries, config keys, roles blocks and binary copies. The binary, config, journal, Node link (and its Node) and plugins stay.

!`relaxkit uninstall $ARGUMENTS`

Show the output exactly as printed.

This command runs outside the sandbox because it changes the machine (`sandbox.excludedCommands`).
