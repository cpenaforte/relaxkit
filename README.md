# relaxkit

relaxkit sets up and manages a curated set of agent tools for Claude Code and OpenCode. It installs the pack (such as rtk, caveman and gh), writes each host's config, sets a model tier or an exact model per role, keeps the tools updated, and can revert its last change. It runs on Linux and macOS.

Status: pre-release. The install line below works only after the owner publishes the first release.

## Install

The line below downloads the installer from the `main` branch. The installer fetches the latest release and checks its SHA-256 checksum. It does not check the minisign signature; the installer prints the command to do that.

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh
```

The script installs `relaxkit` into `$HOME/.local/bin` (set `RELAXKIT_BIN_DIR` to change it), adds an `rlx` alias when that name is free, and runs `relaxkit setup` unless you skip it. To skip setup, add `-s -- --no-setup` after `sh` in the line above. Set `RELAXKIT_VERSION` to install a given release. Setup may ask for your GitHub login.

The script checks the archive's SHA-256 against the release's checksums file. It does not check the minisign signature itself; it prints the command that does.

To update, run the install line again or run `relaxkit update`.

## Quickstart

```sh
relaxkit setup                        # installs the pack and writes the config (balanced preset)
relaxkit status                       # versions, drift and enforced-vs-advisory settings
relaxkit doctor                       # hooks, schemas, sandbox self-test and MCP reachability
relaxkit config set implementer cheap
relaxkit undo                         # reverts the last change
```

Add `--dry-run` to any command to see what it would change without writing anything. On Linux, `setup` prints the sudo command for system packages instead of running it; pass `--allow-sudo` to run that plan. `rlx` is the same binary as `relaxkit`.

## Commands

| Command | What it does |
| --- | --- |
| `relaxkit setup [--allow-sudo]` | Installs prerequisites, the pack, config, sandbox and the GitHub login. |
| `relaxkit status` | Shows versions, drift and enforced-vs-advisory settings. |
| `relaxkit doctor` | Checks hooks, schemas, the sandbox self-test and MCP reachability. Exits 1 when a check fails. |
| `relaxkit update [--self \| --tools] [--check] [TOOL]` | Updates relaxkit itself, then each tool, and updates the lockfile. `--check` only reports available updates. |
| `relaxkit config show [KEY]` | Shows settings, all or one key. |
| `relaxkit config set KEY VALUE [--host claude\|opencode]` | Sets a role to a tier or an exact model. |
| `relaxkit config unset KEY [--host claude\|opencode]` | Removes a setting so the preset value applies again. |
| `relaxkit preset lean\|balanced\|careful` | Applies a preset. Pinned roles keep their pins. |
| `relaxkit privacy [--claude-code]` | Turns off telemetry. See [Privacy](#privacy). |
| `relaxkit login github` | Logs in to GitHub. |
| `relaxkit undo [--run GROUP]` | Reverts the last change, or the named run group. |
| `relaxkit uninstall` | Removes relaxkit and what it installed. |
| `relaxkit usage [ARGS...]` | Runs the usage tool. Arguments go to the tool; a `--help` after the first argument goes to the tool too. |

Global flags go before the command. `usage` passes everything after its first argument to the usage tool, so for `usage` the global flags must come before the command too.

The global flags are:

- `--dry-run` shows what would change without writing anything.
- `--project` applies the change to the current project, where the tool or host supports project scope. `preset`, `privacy` and `uninstall` are global, so they refuse `--project`.
- `--json` prints machine-readable output.

## Pinning a model per role from the CLI

Each role takes either a tier or an exact model.

- Roles: `planner`, `implementer`, `reviewer`, `escalation` and `final-review`.
- Tiers: `cheap`, `standard` and `smarter`. On Claude Code they map to `haiku`, `sonnet` and `opus`.
- Exact model: `provider/model`, with an optional `#variant`. A bare `claude-*` id or one of `haiku`, `sonnet`, `opus` and `fable` means Anthropic.

```sh
relaxkit config set implementer cheap
relaxkit config set reviewer smarter --host opencode
relaxkit config set planner anthropic/claude-sonnet-5-5#high
relaxkit config unset planner
relaxkit config show
```

- Without `--host`, a change covers both hosts. `--host claude` or `--host opencode` limits it to one.
- On Claude Code the `#variant` is ignored. OpenCode gets the full `provider/model#variant`.
- On OpenCode, an exact model must be in the list that `opencode models` prints, or the command refuses it. If that list cannot be read, the value is accepted with a warning.
- On Claude Code, an Anthropic exact model that is not an alias or a `claude-*` id draws a warning. A pin to another provider is not used by Claude Code, and the role keeps its tier there.

Other keys include `voice` and `compact-at`, for example `relaxkit config set compact-at 256k`.

## Privacy

relaxkit has no telemetry of its own, and it has no background update checks: nothing runs on a timer. It uses the network only when you run one of these commands:

- `setup` and `update` download the pack from GitHub, the npm registry, PyPI, nodejs.org and astral.sh, and Playwright's browser download host (for the Chromium that the playwright tool installs).
- `status` reads the npm registry for the sandbox plugin's age. `doctor` does the same, and its sandbox self-test runs the pinned sandbox runtime through npx (which can fetch it from npm) and makes two curl requests (api.github.com and example.com).
- `login github` runs the `gh` CLI, which talks to GitHub.

`relaxkit usage` runs the usage tool with `--offline` added unless you pass that flag yourself.

The telemetry switches relaxkit sets:

- rtk: `rtk telemetry disable`, run by `relaxkit privacy`.
- caveman: `caveman telemetry off`, run by `relaxkit privacy`.
- superpowers: `env.SUPERPOWERS_DISABLE_TELEMETRY = "1"` in Claude Code's settings, written by `setup` and `privacy`.
- Claude Code: `env.DISABLE_TELEMETRY = "1"`, written only by `relaxkit privacy --claude-code`. relaxkit shows a warning first. That switch also stops feature-flag fetching, which removes synced claude.ai skills and plugins and may break Remote Control. relaxkit does not set `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and never sets `DO_NOT_TRACK`.

## Platforms

relaxkit runs on Linux and macOS. The install script supports macOS on Apple silicon and on Intel, and Linux on x86_64 and arm64 (musl builds). Windows is a possible future addition, depending on demand. It is not built.

## Terminal demo

Output of the built binary, `relaxkit --help`:

```text
$ relaxkit --help
Set up and manage a curated set of agent tools

Usage: relaxkit [OPTIONS] <COMMAND>

Commands:
  setup      Install prerequisites, the pack, config, sandbox and the GitHub login
  status     Show versions, drift and enforced-vs-advisory settings
  doctor     Check hooks, schemas, the sandbox self-test and MCP reachability
  update     Update relaxkit itself, then each tool, and update the lockfile
  config     Show, set or unset roles, tiers and other preferences
  preset     Apply a preset
  privacy    Show or change telemetry
  login      Log in to a service
  undo       Revert the last change, or a named run group
  uninstall  Remove relaxkit and what it installed
  usage      Run the usage tool. Tool flags must come after `--` or after the first argument
  help       Print this message or the help of the given subcommand(s)

Options:
      --dry-run  Show what would change without writing anything
      --project  Apply to the current project where the tool or host supports project scope
      --json     Print machine-readable output
  -h, --help     Print help
  -V, --version  Print version
```

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
