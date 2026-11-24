# Getting started with relaxkit

This guide installs relaxkit, sets up the tool pack for Claude Code or OpenCode, checks the result, changes one setting and reverts it. Each step shows the output the command printed. The [command reference](commands.md) has every option, and [troubleshooting](troubleshooting.md) has the messages you may see along the way.

## What is verified

The commands below were checked with relaxkit's binary built from this repository, on macOS, in a disposable home directory. Each captured output is marked with the command that produced it. The installer script was not run for this guide: its behaviour is described from `scripts/install.sh` as it is in this repository. A full install on a fresh Linux or macOS machine has not been tested end to end.

## Before you start

- macOS on Apple silicon or Intel, or Linux on x86_64 or arm64. Windows is not supported.
- `curl`, `tar`, and either `shasum` or `sha256sum`. The installer uses them.
- Claude Code, OpenCode, or both, installed and on your `PATH`. `relaxkit setup` stops if it finds neither (see [Neither host was found](troubleshooting.md#neither-host-was-found)).

`relaxkit setup` installs Node 24 through nvm, uv and gh when they are missing, so you do not need to install them first.

## 1. Install relaxkit

Run the install line. It downloads the installer from the `main` branch, and the installer fetches the latest release.

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh
```

The installer puts `relaxkit` in `$HOME/.local/bin` (`scripts/install.sh` lines 95 to 99), checks the archive against the release's SHA-256 checksums file (lines 121 to 130), and adds the `rlx` alias when that name is free (lines 146 to 155). It does not check the minisign signature; the script prints the command that does (lines 168 to 172). It then runs `relaxkit setup` (lines 157 to 166).

To install without running setup, add `-s -- --no-setup` after `sh`:

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh -s -- --no-setup
```

The installer does not change your `PATH`. If `$HOME/.local/bin` is not on your `PATH`, add it before you run the commands below.

## 2. Check the version

```sh
relaxkit --version
```

```text
relaxkit 0.1.3
```

## 3. Preview setup

A dry run prints the plan and writes nothing. Run it first:

```sh
relaxkit setup --dry-run
```

Output from `relaxkit setup --dry-run` on a machine with Claude Code and OpenCode installed:

```text
·  detect         found Claude Code 2.1.331, OpenCode 2.2.6
·  prerequisites  would install or adopt Node 24 through nvm, uv and gh
·  install        would install or adopt the pack's tools on each detected host, skipping tools already locked
·  configure      would write the balanced preset, the roles, the telemetry switch and the voice
·  sandbox        would write the sandbox settings; Linux packages are printed for you to run unless --allow-sudo is given
·  login          would log in to GitHub if gh is not logged in
·  verify         would run the probes of every locked tool
```

Setup applies the balanced preset on a machine with no config yet, and keeps an existing config's preset and settings. See [relaxkit setup](commands.md#relaxkit-setup).

## 4. Run setup

```sh
relaxkit setup
```

Setup runs its steps in order and prints one line for each. It installs the pack's tools, writes the configuration, turns on the sandbox for Claude Code (see [sandbox.md](sandbox.md)), logs in to GitHub when gh is not logged in, and runs the checks of each locked tool. The full list of steps is in [relaxkit setup](commands.md#relaxkit-setup).

Setup may stop and ask you to do three things by hand. Each message and its fix is in [troubleshooting](troubleshooting.md):

- On Linux, the sandbox needs `bubblewrap` and `socat`. Setup prints one sudo command and asks you to run it, or you can pass `--allow-sudo`. See [Linux packages are missing](troubleshooting.md#linux-packages-are-missing).
- The GitHub login needs a terminal. Run `relaxkit login github` in one. See [Exit code 3](troubleshooting.md#exit-code-3-the-github-login-needs-a-terminal).
- The sandbox self-test runs Claude Code, which must be logged in. Run `claude`, then `/login` inside it. See [The sandbox self-test fails](troubleshooting.md#the-sandbox-self-test-fails).

Setup exits 0 when everything passes. A failed check makes it exit 1, and a pending GitHub login makes it exit 3 (see [exit codes](commands.md#exit-codes)).

## 5. Check the status

```sh
relaxkit status
```

Output from `relaxkit status` before setup has run:

```text
relaxkit 0.1.3 · preset balanced
·  nothing is locked yet; run relaxkit setup

Hosts
  HOST         VERSION
  Claude Code  2.1.331
  OpenCode     2.2.6

Roles
  ROLE          SETTING   CLAUDE CODE        OPENCODE
  planner       smarter   opus · advisory    host default
  implementer   cheap     haiku · advisory   host default
  reviewer      standard  sonnet · advisory  host default
  escalation    standard  sonnet · advisory  host default
  final-review  smarter   opus · advisory    host default

Sandbox
  HOST         SANDBOX         DETAIL
  Claude Code  ✗ not enforced  settings.json lacks the sandbox keys
  OpenCode     ✗ not enforced  the relaxkit-opencode plugin is not listed; run relaxkit setup

Notes
  !  CLAUDE.md roles block is missing; run relaxkit setup to write it
  !  AGENTS.md routing block is missing; run relaxkit setup to write it
  ·  other Node on the PATH: node at $HOME/.nvm/versions/node/v22.22.3/bin/node (not used)

rtk gain: n/a
```

The `not enforced` marks depend on the sandbox settings that setup writes. This capture was taken before setup ran.

## 6. Check the health

```sh
relaxkit doctor
```

`doctor` runs every check and lists the failures and warnings. The sandbox self-test runs for real. Output from `relaxkit doctor --dry-run`, which runs no self-test:

```text
✗  1 failed, 2 warnings, 3 ok

Claude Code  1 warning, 1 ok
  !  claude.sandbox.selftest
     Claude Code: not run in a dry run
     fix  run relaxkit doctor without --dry-run to run the sandbox self-test

OpenCode  1 failed, 1 warning, 1 ok
  !  opencode.sandbox.selftest
     OpenCode: not run in a dry run
     fix  run relaxkit doctor without --dry-run to run the sandbox self-test
  ✗  opencode.sandbox.plugin
     relaxkit-opencode is not in the OpenCode plugins list, so the OpenCode sandbox is not enforced
     fix  run relaxkit setup

3 ok checks hidden; relaxkit doctor --all lists them
```

`doctor` exits 1 when a check fails. Add `--all` to list the checks that passed too. See [relaxkit doctor](commands.md#relaxkit-doctor).

## 7. Change a setting

```sh
relaxkit config set implementer cheap
relaxkit config show implementer
```

`cheap` is a tier. On Claude Code it maps to `haiku`. A dry run of the change on Claude Code only:

```sh
relaxkit config set implementer cheap --host claude --dry-run
```

```text
·  would write $HOME/.config/relaxkit/config.toml
·  would write $HOME/.claude/CLAUDE.md
·  would set implementer to cheap; takes effect in new sessions
   Claude Code  haiku (advisory)
```

Output from `relaxkit config show implementer` before setup has run:

```text
ROLE         SETTING  CLAUDE CODE       OPENCODE
implementer  cheap    haiku (advisory)  inherits the host default (the model list cannot be read)
!  OpenCode's model list could not be read, so OpenCode models show as inherited
```

If OpenCode's model list cannot be read, the change to OpenCode is refused. Use `--host claude` for Claude Code only, as above, or see [OpenCode's model list could not be read](troubleshooting.md#opencodes-model-list-could-not-be-read). The role names, tiers and exact-model form are in [configuration.md](configuration.md).

## 8. Revert the last change

```sh
relaxkit undo
```

Undo reverts the newest journaled change. Output from `relaxkit undo --dry-run` when no change has been journaled:

```text
·  nothing to undo: no changes are journaled
```

See [relaxkit undo](commands.md#relaxkit-undo).

## Next steps

- [Command reference](commands.md): every command, option and exit code.
- [Configuration](configuration.md): roles, tiers, presets and the files relaxkit writes.
- [Sandbox](sandbox.md): what the sandbox allows and which commands run outside it.
- [Privacy](privacy.md): the network use and the telemetry switches.
- [Troubleshooting](troubleshooting.md): the messages you may see, with their causes and fixes.
