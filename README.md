# relaxkit

A curated pack of agent tools for Claude Code and OpenCode: install, configure, update and diagnose from one CLI.

[![CI](https://github.com/cpenaforte/relaxkit/actions/workflows/ci.yml/badge.svg)](https://github.com/cpenaforte/relaxkit/actions/workflows/ci.yml) [![Latest release](https://img.shields.io/github/v/release/cpenaforte/relaxkit)](https://github.com/cpenaforte/relaxkit/releases) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?link=https://github.com/cpenaforte/relaxkit/blob/main/LICENSE-MIT)](#license)

relaxkit is at 0.x, so the command line can change between minor versions. Versions follow [Semantic Versioning](https://semver.org/).

## Highlights

- **One command sets up both hosts.** `relaxkit setup` detects Claude Code and OpenCode, installs the pack's tools for each, and writes their config.
- **A model per role, from the CLI.** Each role (planner, implementer, reviewer, escalation, final-review) takes a tier or an exact model, for example `relaxkit config set implementer cheap`.
- **Undo and uninstall.** `relaxkit undo` reverts the last change. `relaxkit uninstall` removes the pack's MCP entries, config keys, roles blocks and binary copies, and keeps the relaxkit binary, its config and journal, the Node link and the Node it points at, and the pack's plugins. It also removes the lock file, unless it is a symlink, which it keeps. `relaxkit undo` restores a removed lock file.
- **Diagnostics with a sandbox self-test.** `relaxkit doctor` checks hooks, schemas and MCP reachability, and runs a sandbox self-test.
- **Signed releases, no telemetry of its own.** Each release's checksums file is signed with minisign, and relaxkit sends no telemetry of its own.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh
```

The line downloads the installer from the `main` branch. The installer fetches the latest release, checks the archive's SHA-256 against the release's checksums file, and puts `relaxkit` in `$HOME/.local/bin`. It adds an `rlx` alias when that name is free, then runs `relaxkit setup` unless you skip it. Setup may ask for your GitHub login. The installer does not check the minisign signature; it prints the command that does. See [how to verify a release](SECURITY.md#verifying-a-release).

To install without running setup, add `-s -- --no-setup` after `sh`:

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh -s -- --no-setup
```

The installer reads these settings from the environment or its flags:

| Setting | Effect |
| --- | --- |
| `RELAXKIT_VERSION` | Installs this release, such as `0.1.5` or `v0.1.5`. Default: the latest release. |
| `RELAXKIT_BIN_DIR` | Directory for `relaxkit` and the `rlx` alias. Default: `$HOME/.local/bin`. |
| `RELAXKIT_RELEASE_BASE` | Base URL that the release files are downloaded from. Default: GitHub releases. |
| `RELAXKIT_LATEST_URL` | API URL that names the latest release. It is read only when `RELAXKIT_VERSION` is not set. |
| `--no-setup` | Installs the binary and stops, without running `relaxkit setup`. |

To update, run the install line again or run `relaxkit update`.

### Build from source

Install Rust 1.96, the toolchain that CI uses (see [CONTRIBUTING.md](CONTRIBUTING.md#development-setup)). Then build the release binary:

```sh
cargo build --release
```

The binary is written to `target/release/relaxkit`.

## Quickstart

```sh
relaxkit setup                        # installs the pack and writes the config (balanced preset)
relaxkit status                       # versions, drift and enforced-vs-advisory settings
relaxkit doctor                       # hooks, schemas, sandbox self-test and MCP reachability
relaxkit config set implementer cheap
relaxkit undo                         # reverts the last change
```

Add `--dry-run` to any command to see what it would change without writing anything. This is the output of `relaxkit setup --dry-run` on a machine with Claude Code and OpenCode installed, run with an empty home directory:

```text
$ relaxkit setup --dry-run
·  detect         found Claude Code 2.1.331, OpenCode 2.2.6
·  prerequisites  would install or adopt Node 24 through nvm, uv and gh
·  install        would install or adopt the pack's tools on each detected host, skipping tools already locked
·  configure      would write the balanced preset, the roles, the telemetry switch and the voice
·  sandbox        would write the sandbox settings; Linux packages are printed for you to run unless --allow-sudo is given
·  login          would log in to GitHub if gh is not logged in
·  verify         would run the probes of every locked tool
```

On Linux, `setup` prints the sudo command for the system packages instead of running it. Pass `--allow-sudo` to run that plan.

## Commands

| Command | What it does |
| --- | --- |
| [relaxkit setup](docs/commands.md#relaxkit-setup) | Installs the prerequisites, the pack and the config, writes the sandbox settings, and logs in to GitHub if needed. |
| [relaxkit status](docs/commands.md#relaxkit-status) | Shows versions, drift and enforced-vs-advisory settings. |
| [relaxkit doctor](docs/commands.md#relaxkit-doctor) | Checks hooks, schemas, the sandbox self-test and MCP reachability. |
| [relaxkit update](docs/commands.md#relaxkit-update) | Updates relaxkit itself, then each tool, and the lockfile. |
| [relaxkit config](docs/commands.md#relaxkit-config) | Shows, sets or unsets roles, tiers and other preferences. |
| [relaxkit preset](docs/commands.md#relaxkit-preset) | Applies the lean, balanced or careful preset. |
| [relaxkit privacy](docs/commands.md#relaxkit-privacy) | Shows or changes telemetry. |
| [relaxkit login](docs/commands.md#relaxkit-login) | Logs in to a service, such as GitHub. |
| [relaxkit undo](docs/commands.md#relaxkit-undo) | Reverts the last change, or a named run group. |
| [relaxkit uninstall](docs/commands.md#relaxkit-uninstall) | Removes the pack's MCP entries, config keys, roles blocks and binary copies. The binary, config, journal, Node link (and its Node) and plugins stay. It also removes the lock file, unless it is a symlink, which it keeps. `relaxkit undo` restores a removed lock file. |
| [relaxkit usage](docs/commands.md#relaxkit-usage) | Runs the usage tool with the arguments you give it. |

The global flags and every option are in the [command reference](docs/commands.md). Model pinning, tiers and presets are in [configuration](docs/configuration.md).

## Documentation

**Using relaxkit**

- [Getting started](docs/getting-started.md): install, the first run, and one change that is reverted.
- [Command reference](docs/commands.md): every command, option and output mark.
- [Configuration](docs/configuration.md): roles, tiers, exact models, presets and the files relaxkit writes.
- [Tools](docs/tools.md): every tool in the pack and where it comes from.

**Trust and safety**

- [Privacy](docs/privacy.md): every network request and telemetry switch.
- [Sandbox](docs/sandbox.md): the commands that run outside the sandbox, and why.
- [Verification](docs/verification.md): the checks behind the pack's success criteria, with their results.

**Project**

- [Changelog](CHANGELOG.md): what changed in each release.
- [Contributing](CONTRIBUTING.md): the development setup, the checks and the pull request process.
- [Security policy](SECURITY.md): supported versions, private reporting and how to verify a release.
- [Code of conduct](CODE_OF_CONDUCT.md): the standards for taking part and how to report a problem.

## Platforms

relaxkit runs on Linux and macOS. The install script supports macOS on Apple silicon and on Intel, and Linux on x86_64 and arm64 (musl builds). Windows is a possible future addition, depending on demand. It is not built.

## Getting help

Start with [troubleshooting](docs/troubleshooting.md), which lists the messages relaxkit prints, their causes and their fixes. To report a bug or request a feature, open an issue on [GitHub](https://github.com/cpenaforte/relaxkit/issues). To report a security vulnerability, follow [SECURITY.md](SECURITY.md) and report it privately; do not open a public issue for it.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the checks that CI runs and how to open a pull request. Everyone who takes part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Changelog

Release notes are in [CHANGELOG.md](CHANGELOG.md).

## License

relaxkit is dual-licensed under MIT OR Apache-2.0, as set in `Cargo.toml`. The license texts are in [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

The OpenCode sandbox plugin in [adapters/opencode-plugin](adapters/opencode-plugin/) contains a fork of the hook logic of an MIT-licensed project; its notice is in [NOTICE](adapters/opencode-plugin/NOTICE.md).
