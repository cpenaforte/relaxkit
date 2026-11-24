# relaxkit tools

This page lists every tool in the pack: what it is, which hosts it installs on, and where it comes from. Each tool is defined by a recipe in the repository's `recipes/` folder. The prerequisites that `relaxkit setup` installs, Node through nvm, uv and gh, are not recipes, so they are not in this table. The version that is installed on a machine is recorded in `lock.toml`, described in [Files relaxkit writes](configuration.md#files-relaxkit-writes).

## The pack

| `id` | What it is | Claude Code | OpenCode | Installed from |
| --- | --- | --- | --- | --- |
| `caveman` | A terse output style for the agent, and a CLI that wraps coding agents with local context compression. | Plugin, and CLI | Files, and CLI | npm package `@caveman-ai/cli`. The Claude Code plugin and the OpenCode files come from the `JuliusBrussee/caveman` GitHub repository. Newest release at install time. |
| `ccusage` | Reports coding agent token use and cost from local data. | Run by `relaxkit usage` | Run by `relaxkit usage` | npm package `ccusage`, run with npx at the version the lock names, and with `--offline`. No global install: npx keeps the package in its cache, `~/.npm/_npx`. Newest release at install time. |
| `chrome-devtools` | An MCP server that lets the agent control and inspect a live Chrome browser. | MCP server | MCP server | npm package `chrome-devtools-mcp`, run with npx. It requires `playwright`, which installs the Chromium it uses. Newest release at install time. |
| `claude-md-management` | Audits CLAUDE.md files against the codebase, and captures what a session learned into them. | Plugin | No | Claude Code plugin `claude-md-management`, from a listed marketplace that already lists it. If none does, the `claude-plugins-official` marketplace (`anthropics/claude-plugins-official`) is added and used, unless it is already listed. Newest release at install time. |
| `frontend-design` | Generates frontend interfaces with distinctive styling, avoiding generic defaults. | Plugin | No | Claude Code plugin `frontend-design`, from a listed marketplace that already lists it. If none does, the `claude-plugins-official` marketplace (`anthropics/claude-plugins-official`) is added and used, unless it is already listed. Newest release at install time. |
| `playwright` | An MCP server that lets the agent drive a browser through Playwright's accessibility snapshots. | MCP server, and Chromium | MCP server, and Chromium | npm package `@playwright/mcp`, run with npx, and the Chromium that Playwright downloads for it. Newest release at install time. |
| `rtk` | A command-output compressor that shrinks what shell commands return before the agent reads it. | Binary, with a Claude Code hook | Binary, configured by `rtk init` | Binary from the GitHub release of `rtk-ai/rtk`. Newest release at install time. |
| `serena` | Symbol-level code retrieval, editing and refactoring tools, served as an MCP server. | MCP server | MCP server | PyPI package `serena-agent`, installed with uv. Newest release at install time. |
| `superpowers` | A software development method made of composable skills and instructions for coding agents. | Plugin, from the built-in `anthropic-plugin-directory` marketplace | Plugin, from a git tag of `obra/superpowers` | Claude Code: the `superpowers` plugin from the built-in `anthropic-plugin-directory` marketplace. OpenCode: a git tag of the `obra/superpowers` GitHub repository. Newest release at install time. |

## Reading the table

- **Plugin** is a Claude Code plugin, installed through `claude plugin`.
- **MCP server** is an MCP entry that relaxkit adds to the host.
- **No** means the recipe lists only the other host, so the tool is not installed on that host.
- **Installed from** names the package kind and its source. Every row takes the newest release at install time, so no version is written here. The exact version and route that were installed are recorded in `lock.toml`.
- `relaxkit update` updates each tool to its newest release, then rewrites `lock.toml`.
