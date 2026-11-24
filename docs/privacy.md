# relaxkit privacy

This page says what relaxkit sends over the network, which commands do that, and which telemetry switches it sets. The [command reference](commands.md) has the syntax of each command.

## What relaxkit does not do

relaxkit has no telemetry of its own. It has no background update checks: nothing runs on a timer, and relaxkit makes a request only while one of its commands runs. Its requests are plain GET requests, and the only header it sets is the User-Agent, `relaxkit/<version>`.

## When relaxkit uses the network

| Command | Network use |
| --- | --- |
| `setup` | Installs the prerequisites and the pack's tools. The hosts are listed below. |
| `update` | Looks up the newest version of each tool, then updates each tool that has a newer version. `update --check` and `update --dry-run` make the lookups and stop there. |
| `update --self` | Downloads relaxkit's own release from GitHub, and checks it before it replaces the binary. |
| `status` | Reads the sandbox runtime's npm entry and the date of relaxkit's latest release, but only when `relaxkit-opencode` is listed in OpenCode. |
| `doctor` | Runs the sandbox self-test, which is not run in a dry run. When `relaxkit-opencode` is listed in OpenCode, it also makes the two reads that `status` makes. |
| `login github` | Runs `gh`, which talks to GitHub. If `gh` is missing, it is installed first. |
| `usage` | Runs ccusage through npx with `--offline` added, so ccusage does not fetch its pricing data. npx can download ccusage from npm the first time it runs. |

`privacy`, `config`, `preset`, `undo` and `uninstall` make no request of their own. `config set` with an exact model runs `opencode models --standalone` to check the model name. That is OpenCode's own process, so its network use is OpenCode's.

The probes that `setup` and `doctor` run for each tool are commands of the tool. For an npm tool, a probe runs npx, which can download the package from npm when it is not in npm's cache.

### Hosts that setup and update contact

These are the hosts that relaxkit's own requests and installs reach. The host CLIs that relaxkit runs (Claude Code, OpenCode, npm, npx, uv and gh) make their own requests, which relaxkit does not control.

- `api.github.com`: the newest release tag of gh, rtk, caveman, superpowers and relaxkit.
- `github.com`: the release archives and checksums of gh and rtk, and relaxkit's release for `update --self`. It is also the source of the superpowers and `relaxkit-opencode` plugins, of the Claude Code marketplaces that relaxkit adds when they are missing, and of the GitHub login.
- `codeload.github.com`: caveman's source archive for its release tag.
- `raw.githubusercontent.com`: the nvm install script. It must match its pinned SHA-256 before it runs.
- `nodejs.org`: the Node release index, which selects the Node 24 release. nvm then downloads Node from nodejs.org by default.
- `astral.sh`: the uv install script. It must match its pinned SHA-256 before it runs.
- `registry.npmjs.org`: the newest version of each npm tool, the npm installs and the npx runs. `status` and `doctor` also read the sandbox runtime's entry here.
- `pypi.org`: the newest version of `serena-agent`. uv then installs it from PyPI's file host, `files.pythonhosted.org`. If uv does not have Python 3.13, it fetches a Python build of its own.
- Playwright's browser download host: the Chromium build for the Playwright MCP server. `setup` runs `install-browser chromium` through npx, and Playwright chooses the host. relaxkit does not name it.

## Telemetry switches

relaxkit turns off the telemetry of the pack's own tools and of Claude Code's superpowers plugin. These are the switches it sets:

- rtk: `rtk telemetry disable`. `relaxkit privacy` runs it, and `setup` runs it when it installs rtk.
- caveman: `caveman telemetry off`. `relaxkit privacy` runs it, and `setup` runs it when it installs caveman.
- superpowers: `env.SUPERPOWERS_DISABLE_TELEMETRY = "1"` in Claude Code's settings file, `~/.claude/settings.json`. `setup` writes it on each run for each Claude Code host it finds, and `relaxkit privacy` writes it too.
- Serena: `SERENA_USAGE_REPORTING=false` in the environment of its MCP entry, for Claude Code and for OpenCode. `setup` writes it when it installs Serena.
- chrome-devtools-mcp: `CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS=1` in the environment of its MCP entry. This turns off the server's update checks, not a telemetry feature.
- Claude Code: `env.DISABLE_TELEMETRY = "1"`. Only `relaxkit privacy --claude-code` writes it.

The `DISABLE_TELEMETRY` switch also stops feature-flag fetching. That removes synced claude.ai skills and plugins and `/skill-doctor`, and it may break Remote Control. `relaxkit privacy --claude-code` shows this warning before it writes the switch. Claude Code's separate `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` switch also turns off auto-updates, and relaxkit does not set it. relaxkit never sets `DO_NOT_TRACK`, because Claude Code also reads that variable and it turns off the feature flags too.

## relaxkit privacy

```text
relaxkit privacy [--claude-code]
```

Without the flag, privacy turns off rtk and caveman telemetry, and it writes the superpowers telemetry switch to Claude Code's settings. With `--claude-code` it also writes `env.DISABLE_TELEMETRY = "1"` to Claude Code's settings. That switch also stops feature-flag fetching, which removes synced claude.ai skills and plugins. Privacy prints a warning about this with its plan.

`relaxkit privacy` runs the rtk and caveman commands, so it stops with an error when one of them fails. `relaxkit privacy --claude-code` stops with an error when Claude Code is not found.

To see the plan without changing anything:

```sh
relaxkit privacy --dry-run
relaxkit privacy --claude-code --dry-run
```
