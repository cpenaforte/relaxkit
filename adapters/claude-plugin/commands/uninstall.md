---
description: Remove the MCP entries, config keys, roles blocks and binary copies the pack wrote, and the lock file unless it is a symlink; keep the relaxkit binary, its config and journal, the Node link and the pack's plugins
allowed-tools: Bash(relaxkit:*)
---

Remove the pack's MCP entries, config keys, roles blocks and binary copies. The binary, config, journal, Node link (and its Node) and plugins stay.

!`relaxkit uninstall $ARGUMENTS`

Show the output exactly as printed.

This command runs outside the sandbox because it changes the machine (`sandbox.excludedCommands`).
