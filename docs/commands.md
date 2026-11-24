# relaxkit command reference

This page lists every relaxkit command, its options, what it does, and its exit codes. The [README](../README.md) gives the short version.

## How to read a synopsis

Each command has a synopsis in a `text` block. The marks in a synopsis mean:

- `[X]` is optional.
- `A|B` means one of the choices.
- `KEY`, `VALUE`, `TOOL` and `GROUP` are values you supply.
- `[ARGS]...` is any number of arguments.

The synopses leave out the global flags, which apply to every command (see [Global flags](#global-flags)). The examples in `sh` blocks are real commands that you can copy.

`rlx` is the same binary as `relaxkit`. The installer adds it as an alias when that name is free.

## Commands

| Command | What it does |
| --- | --- |
| [setup](#relaxkit-setup) | Install prerequisites, the pack, config, sandbox and the GitHub login |
| [status](#relaxkit-status) | Show versions, drift and enforced-vs-advisory settings |
| [doctor](#relaxkit-doctor) | Check hooks, schemas, the sandbox self-test and MCP reachability |
| [update](#relaxkit-update) | Update relaxkit itself, then each tool, and update the lockfile |
| [config](#relaxkit-config) | Show, set or unset roles, tiers and other preferences |
| [preset](#relaxkit-preset) | Apply a preset |
| [privacy](#relaxkit-privacy) | Show or change telemetry |
| [login](#relaxkit-login) | Log in to a service |
| [undo](#relaxkit-undo) | Revert the last change, or a named run group |
| [uninstall](#relaxkit-uninstall) | Remove the pack's MCP entries, config keys, roles blocks and binary copies. |
| [usage](#relaxkit-usage) | Run the usage tool |

## Global flags

These flags apply to every command. They work before or after the subcommand, for example `relaxkit --json doctor` and `relaxkit doctor --json` are the same. The exception is `usage`, which reads them only before its first argument.

- `--dry-run` shows what would change without writing anything. A dry run changes no file that relaxkit manages.
- `--project` applies a `config` change to the current project. `doctor` and `config` read it. `preset`, `privacy` and `uninstall` refuse it with an error, because they are global. The other commands ignore it.
- `--json` prints a JSON document for `doctor`, `update` and `config show`. The other commands print their text with or without it.
- `-h, --help` prints the help, and `-V, --version` prints the version.

## relaxkit setup

```text
relaxkit setup [--allow-sudo]
```

Options:

- `--allow-sudo` runs the sandbox's sudo plan for the Linux packages. Without it, setup prints the command for you to run.

Setup detects Claude Code and OpenCode, installs the prerequisites (Node through nvm, uv and gh) and the pack's tools, and writes the sandbox settings. On a machine with no config yet it writes the balanced preset; an existing config keeps its preset and settings. Setup then logs in to GitHub if gh is not logged in, and last runs the probes of every locked tool.

```sh
relaxkit setup --dry-run
relaxkit setup --allow-sudo
```

## relaxkit status

```text
relaxkit status
```

Status prints the versions of the hosts, the setting of each role on each host, and the sandbox state of each host, with notes where something is missing. It only reads, and it exits 0.

```sh
relaxkit status
```

## relaxkit doctor

```text
relaxkit doctor [--all]
```

Options:

- `--all` also lists the checks that passed. By default only the failures and warnings are listed, and the passing checks are counted.

Doctor runs the checks for hooks, schemas, the sandbox self-test and MCP reachability on each host. Each failure and warning has a `fix` line that names the command to run. In a dry run the sandbox self-test is not run. Doctor exits 1 when a check fails; warnings alone exit 0.

```sh
relaxkit doctor --all
```

## relaxkit update

```text
relaxkit update [--self | --tools] [--check] [TOOL]
```

Options:

- `--self` updates only relaxkit itself.
- `--tools` updates only the tools.
- `--check` reports the available updates without installing them.
- `TOOL` updates one named tool.

`--self` cannot be combined with `--tools` or `TOOL`, and `--tools` cannot be combined with `TOOL`.

A plain update updates relaxkit itself first, then each tool, then the lockfile. A failed self step does not stop the tool phase. Update prints the tools as a table with the installed and newest versions. When the newest version cannot be fetched it shows `unknown`. Update exits 1 when relaxkit itself or a tool failed. In a dry run each tool shows `not touched`.

```sh
relaxkit update --check
relaxkit update rtk
```

## relaxkit config

```text
relaxkit config show [KEY]
relaxkit config set KEY VALUE [--host claude|opencode]
relaxkit config unset KEY [--host claude|opencode]
```

Options:

- `set` takes a role (`planner`, `implementer`, `reviewer`, `escalation` or `final-review`) or another key, such as `voice` or `compact-at`. For a role, `VALUE` is a tier (`cheap`, `standard` or `smarter`) or an exact model. For `voice`, `VALUE` is `caveman`, `ultracave`, `megacave` or `off`. For `compact-at`, `VALUE` is a token count such as `256k`, `1m` or `256000`.
- `unset` removes a setting, so the preset's value applies again.
- `show` prints all settings, or one key when you give `KEY`.
- `--host claude` or `--host opencode` limits a role change to one host. `--host` applies to roles only: `voice` and `compact-at` refuse it, because they apply to both hosts.

Each role takes either a tier or an exact model. The tiers map to `haiku`, `sonnet` and `opus` on Claude Code. An exact model is `provider/model`, with an optional `#variant`, and a bare `claude-*` id, or the bare name `haiku`, `sonnet`, `opus` or `fable`, means Anthropic.

- Without `--host`, a change covers both hosts.
- On Claude Code the `#variant` is ignored. OpenCode gets the full `provider/model#variant`.
- On OpenCode, an exact model must be in the list that `opencode models --standalone` prints, or the command refuses it. If that list cannot be read, the exact model is accepted with a warning.
- A tier for OpenCode needs that same list. When it cannot be read, `config set` and `config unset` refuse the tier and ask for an exact model instead, so use `--host claude` or pin an exact model.

```sh
relaxkit config show
relaxkit config set implementer cheap --host claude
relaxkit config set reviewer smarter --host opencode
relaxkit config set planner anthropic/claude-sonnet-5-5#high
relaxkit config unset planner --host claude
relaxkit config set compact-at 256k
```

## relaxkit preset

```text
relaxkit preset lean|balanced|careful
```

Preset applies one of the three presets: its tiers and its voice, on each detected host. Roles pinned to an exact model keep their pins, and tiers set by hand are cleared so the preset's tiers apply. A preset is global, so `--project` is refused. Like `config set`, it refuses a tier for OpenCode when OpenCode's model list cannot be read.

```sh
relaxkit preset balanced
```

## relaxkit privacy

```text
relaxkit privacy [--claude-code]
```

Options:

- `--claude-code` also turns off Claude Code's own telemetry.

Without the flag, privacy turns off rtk and caveman telemetry, and it writes the superpowers telemetry switch to Claude Code's settings. With `--claude-code` it also writes `env.DISABLE_TELEMETRY = "1"` to Claude Code's settings. That switch also stops feature-flag fetching, which removes synced claude.ai skills and plugins. Privacy prints a warning about this with its plan.

```sh
relaxkit privacy --dry-run
```

## relaxkit login

```text
relaxkit login github
```

Login runs `gh auth login --web --clipboard` and then `gh auth setup-git`, unless `gh auth status` already passes. If gh is missing, login installs it first. relaxkit never reads or writes a GitHub token. Without a terminal, when gh is not logged in, login prints the command that finishes the login by hand and exits 3.

```sh
relaxkit login github --dry-run
```

## relaxkit undo

```text
relaxkit undo [--run GROUP]
```

Options:

- `--run GROUP` reverts the named run group instead of the newest one.

Undo reverts the newest journaled change, or the run group you name. When nothing is journaled it says so and exits 0.

```sh
relaxkit undo --dry-run
```

## relaxkit uninstall

```text
relaxkit uninstall
```

Uninstall removes the MCP entries, config keys and roles blocks the pack wrote, and the pack's binary copies under `dirs.bin`. It keeps the relaxkit binary, its config and journal (so `relaxkit undo` still works), the Node link with the Node it points at, and the pack's plugins. Uninstall also removes the lock file, which `relaxkit undo` restores. The dry run lists each of these, so you can see what stays. Uninstall refuses `--project`.

```sh
relaxkit uninstall --dry-run
```

## relaxkit usage

```text
relaxkit usage [ARGS]...
```

Usage runs the usage tool (ccusage) at the version that the lockfile names, through the Node that relaxkit installed. The arguments pass to the tool unchanged, and relaxkit adds the tool's `--offline` flag unless you pass it. Usage needs `relaxkit setup` to have run first, and stops with an error if the tool is not locked. Its exit code is the tool's exit code. Before the first argument, `-h` and `--help` are relaxkit's own, and the global flags are read; after it, they go to the tool.

```sh
relaxkit usage daily
```

## Exit codes

| Code | Meaning | Where it comes from |
| --- | --- | --- |
| 0 | The command finished. A warning alone does not change the code. | `src/lib.rs`, the `Ok(0)` returns |
| 1 | A check or a step failed: `doctor` (a check failed), `update` (relaxkit or a tool failed), or `setup` (a verify check failed). Any other error also exits 1, and prints `Error:` on standard error. | `src/lib.rs` for `doctor` and `update`; `FAILED_CHECK_EXIT` in `src/commands/setup.rs`; the error arm in `src/main.rs` |
| 2 | A usage error: an unknown command, flag or value, or a missing argument. | clap, through `Cli::parse()` in `src/main.rs` |
| 3 | A manual step remains: you must finish the GitHub login, then run the command again. | `MANUAL_STEP_EXIT_CODE` in `src/github.rs` for `login github`; `LOGIN_PENDING_EXIT` in `src/commands/setup.rs` for `setup` |
| the tool's code | `usage` exits with the code of the usage tool. | `src/commands/usage.rs` |

In `setup`, a failed check (1) takes precedence over a pending login (3).

## Reading the output

Every command marks its result lines the same way. A line starts with a mark: `✓` done, `!` done with something to check, `✗` failed, and `·` not done. A dry run prints its plans as `·` lines, because nothing is changed. A check that cannot run in a dry run, such as the sandbox self-test in `doctor`, shows `!`. Without a UTF-8 locale the marks are `ok`, `!!`, `xx` and `..`.

`doctor` starts its text with a verdict line, and `status` prints one on the line after its heading. `doctor` groups its failures and warnings by area and gives a `fix` line for each. `setup` prints one line per step. `update` lists the tools in a table.

Colour appears only on a terminal, and a non-empty `NO_COLOR` turns it off. A pipe or a file gets plain text. On a terminal a long step (`setup`, `update`, `uninstall` and `doctor`) shows a spinner until it ends. With `--json`, `doctor`, `update` and `config show` print one JSON document on standard output and draw no spinner.

## Environment variables

These variables change how relaxkit runs:

- `NO_COLOR`: when it is set to a non-empty value, relaxkit prints no colour.
- `TERM=dumb`: relaxkit prints no colour and draws no spinner.
- `LC_ALL`, `LC_CTYPE` or `LANG` with a UTF-8 charset: relaxkit uses the Unicode marks. Otherwise it uses the ASCII marks.

The installer reads these variables, which are listed in the header of `scripts/install.sh`:

- `RELAXKIT_VERSION` is the release to install, such as `1.2.3` or `v1.2.3`. The default is the latest release.
- `RELAXKIT_BIN_DIR` is where relaxkit and the `rlx` alias go. The default is `$HOME/.local/bin`.
- `RELAXKIT_RELEASE_BASE` is the download base. The installer fetches `<base>/v<version>/<file>`. The default is GitHub's release download location.
- `RELAXKIT_LATEST_URL` is the API URL that names the latest release. The installer reads it only when no version is set.

## Slash commands

In Claude Code, `relaxkit setup` is expected to install the relaxkit plugin, which adds one slash command per subcommand under the plugin namespace, `/relaxkit:<subcommand>`. This form is not yet confirmed in a live session (see the last paragraph of this section):

- `/relaxkit:setup`, `/relaxkit:status`, `/relaxkit:doctor`, `/relaxkit:update`, `/relaxkit:config`, `/relaxkit:preset`, `/relaxkit:privacy`, `/relaxkit:login`, `/relaxkit:undo`, `/relaxkit:uninstall` and `/relaxkit:usage`.

Each slash command is a thin wrapper. It runs `relaxkit <subcommand>` with the arguments you give after the name, and shows the output as printed. The commands that change the machine (`setup`, `update`, `config`, `preset`, `privacy`, `login`, `undo`, `uninstall` and `doctor`) run outside the sandbox, because they change the machine (`sandbox.excludedCommands`). `status` and `usage` stay sandboxed.

```text
/relaxkit:config set implementer cheap
/relaxkit:config set planner anthropic/claude-sonnet-5-5#high
/relaxkit:config set reviewer smarter --host claude
```

The examples are the same as the `config` examples above, in slash form: a tier, and an exact model in `provider/model#variant` form. `--host claude|opencode` limits a role change to one host, and `--project` applies a `config` change to the current project.

The slash form, `/relaxkit:<subcommand>`, is not yet confirmed in a live Claude Code session. Until it is confirmed, use `relaxkit <subcommand>` in a terminal.

## relaxkit --help

The output of `relaxkit --help` from the built binary:

```text
A curated pack of agent tools for Claude Code and OpenCode: install, configure, update and diagnose from one CLI.

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

Examples:
  relaxkit setup --dry-run
  relaxkit status
  relaxkit config set implementer cheap
  relaxkit doctor --all
  relaxkit undo
Documentation: https://github.com/cpenaforte/relaxkit/tree/main/docs
```
