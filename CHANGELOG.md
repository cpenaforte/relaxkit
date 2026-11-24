# Changelog

All notable changes to relaxkit are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.5] - 2026-11-24

### Changed

- The OpenCode plugin takes the newest sandbox runtime 0.x when it is installed, instead of 0.0.83. `relaxkit doctor` reports the installed runtime.

## [0.1.4] - 2026-11-24

### Changed

- `relaxkit --help` ends with examples and a link to the documentation, and the help for `setup`, `update` and `config` ends with examples.
- The description in `relaxkit --help` now reads "A curated pack of agent tools for Claude Code and OpenCode: install, configure, update and diagnose from one CLI." instead of "Set up and manage a curated set of agent tools".
- A recipe that sets a workdir is refused with a message that names the recipe and says relaxkit cannot apply the workdir yet.
- The Claude Code plugin's description now matches the description in `relaxkit --help`.
- The `/relaxkit:uninstall` command's description now says what uninstall removes and what it keeps.
- The `relaxkit --help` entry for `uninstall` now names what it removes: the pack's MCP entries, config keys, roles blocks and binary copies.
- The OpenCode plugin's description of the shell `gh` command now says that the sandbox denies it `~/.config/gh`, instead of that it cannot reach GitHub.
- The Homebrew formula's description matches the product sentence.

## [0.1.3] - 2026-11-23

### Changed

- The sandbox self-test failure message now tells you to log in to Claude Code first. The probes run
  Claude Code, which does not answer until it is logged in.

### Fixed

- On Linux arm64, relaxkit finds Chromium in the `chrome-linux-arm64` folder that Playwright unpacks it
  to, so `relaxkit setup` no longer stops at the browser step on a fresh arm64 Linux install.

## [0.1.2] - 2026-11-23

### Changed

- Tool recipes name no version. An install takes the newest release the registry reports, and a repair
  with no lock entry does the same. Tools that are already present are recognized without a registry
  lookup, so that check needs no network.
- nvm v0.41.0 and uv 0.13.9 stay at those versions. Node stays on major 24 and moves to its newest 24.x
  release.
- `relaxkit undo` names each Claude marketplace that a run added, with the command that removes it. It
  does not remove the marketplace itself.

### Fixed

- `relaxkit update` compares the version each host reports, not the lock entry, so a host behind the
  locked version is no longer shown as current.
- An npm tool on OpenCode reports the version its package has in the node tree. When no version can be
  read, the tool is adopted as unversioned and is not written to the lock.
- Removing a managed block from a file with no final newline leaves the file byte-exact.

## [0.1.1] - 2026-11-23

### Fixed

- When no listed Claude marketplace has the plugin, `relaxkit setup` adds the official
  `claude-plugins-official` marketplace instead of failing. A dry run plans the add, and a failed add
  names the marketplace.
- The OpenCode model list is read with `opencode models --standalone`, so it works while another
  OpenCode service holds port 49374.
- Local OpenCode plugins, such as `plugins/caveman` and `plugins/rtk.ts`, are seen by `status` and
  `update`, so they no longer show as not found. `doctor` matches the same names.
- When the two hosts of a tool report different versions, the lock records each host's own version,
  and each per-host read shows that host's version. Hosts that agree keep the lock text unchanged. A
  v0.1.0 install may keep showing OpenCode drift for `superpowers` until `relaxkit update` moves the
  tool, or `relaxkit uninstall` followed by `relaxkit setup` runs.
- A tool that runs through npx with no install step, such as ccusage, shows as "runs through npx" in
  `status`, not as not found.
- `relaxkit uninstall` removes the `CLAUDE.md` that setup created and left empty, and removes
  `lock.toml`. It leaves symlinks alone. Both files are backed up first, so `relaxkit undo` restores
  them. A dry run changes nothing, and after uninstall `status` reports no tools.
- `doctor` and the setup verify step run the npx probes with the pack's own npx and fill in
  `{version}`, instead of printing "not run here". When the pack's npx is missing, a probe now fails
  instead of warning.

## [0.1.0] - 2026-11-22

### Added

- `scripts/install.sh` installs the release binary for each of the four targets. It checks the archive's
  SHA-256 before anything is installed, adds the `rlx` alias when that name is free, and runs
  `relaxkit setup` unless `--no-setup` is given.
- `relaxkit setup` runs the detect, prerequisites, install, configure, sandbox, login and verify steps in
  order, and prints one line per step. A missing host stops it with a hint, and `--dry-run` prints the
  plan and writes nothing.
- Setup installs the pack's tools on each host. It detects each tool, adopts an existing install instead
  of reinstalling it, and records each install in `lock.toml`. A failed step stops that tool and leaves
  the lock unchanged.
- `relaxkit setup` turns off Superpowers telemetry on each Claude Code host on every setup run. With no
  config file, it applies the balanced preset and its caveman voice. An existing config file is kept,
  and its preset and voice are applied.
- When it runs again, `relaxkit setup` repairs a failing probe or MCP command. It reinstalls the locked
  tool and checks again. A repair that still fails leaves the check failed, with its fix line, and setup
  exits 1.
- `relaxkit doctor` checks that each locked MCP entry is in the host's config. A missing entry is a failed
  check, and setup re-adds it.
- `relaxkit doctor` checks that each locked MCP command resolves on the MCP PATH, without starting the
  server. A missing command is a failed check with a fix line.
- `relaxkit status` shows the preset, the hosts, the installed tools against their locked versions, and
  the sandbox state, and writes nothing. `relaxkit doctor` runs the health checks, prints them as JSON
  with `--json`, and exits 1 when a check fails.
- `relaxkit update` moves each tool along its own route. `--check` changes nothing, `--tools` covers the
  tools, and a tool name updates that one tool. A tool is probed after it moves and returns to its locked
  version when the probe fails. Node moves within major 24 only, and nvm and uv stay pinned.
- `relaxkit update` also updates relaxkit itself. It checks the newest GitHub release, verifies the
  checksums file with the embedded minisign key and the archive's SHA-256, then replaces the binary and
  keeps the previous one as `relaxkit.prev`. A binary installed by Homebrew is updated with `brew upgrade`.
- `relaxkit config` sets, unsets and shows the role pins, voice and compact threshold, with `--host` to
  limit a role to one host and `--project` to write project files. `relaxkit preset` applies the tiers
  and voice and keeps the pins. `relaxkit privacy` turns off the pack tools' telemetry.
- `relaxkit undo` reverts the changes of one run, and `relaxkit uninstall` removes only what relaxkit
  wrote and lists what stays in place. `relaxkit usage` runs the locked ccusage.
- `relaxkit login github` runs the GitHub web login and sets git to use gh. It is skipped when
  `gh auth status` already passes. Without a terminal it prints the command and exits 3. Claude Code
  reaches gh outside its sandbox through an exclusion, so relaxkit writes no token.
- Setup writes the Claude Code sandbox settings and runs the sandbox self-test. On Linux the sandbox
  packages run with sudo only when `--allow-sudo` is given, and otherwise the command is printed.
- The Claude Code plugin: the repository carries the marketplace file and the plugin manifest. Setup adds
  the marketplace and installs the `relaxkit` plugin on each detected Claude Code host, unless it is
  already listed. The plugin also ships slash-command files for Claude Code.
- relaxkit ships its own OpenCode sandbox plugin, `relaxkit-opencode`, in place of the upstream one.
  Setup installs it on each OpenCode host and stops if that fails, and `status` and `doctor` report its
  health.
- Every command prints its results with the same marks: `✓` done, `!` done with something to check,
  `✗` failed, and `·` not done. In a dry run every line is a `·` plan, so no dry-run line claims a
  change.
- `doctor` lists failures and warnings by area, with the command for each. `doctor --all` also lists the
  checks that passed.
- `setup` prints one line per step, `status` and `config show` print tables, `update` prints a table of
  tools, and `uninstall` lists what it left in place.
- A failed host command shows the command, its cleaned output (at most six lines), and a hint when the
  cause is known.
- Long steps (`setup`, `update`, `uninstall` and `doctor`) show a spinner on a terminal.
- Color appears only on a terminal, and `NO_COLOR` turns it off. Pipes and `--json` get plain text with
  no escape codes. Without a UTF-8 locale the marks are ASCII.
- Warnings go to standard error with the `!` mark.
- Release pipeline (`.github/workflows/release.yml`): a `v*` tag builds the release archives for each
  target, including the musl targets, writes a checksums file, signs it with minisign, and publishes the
  GitHub release only after signing.
- Each release attaches `relaxkit.rb`, the Homebrew formula, which installs `rlx` as a symlink to
  `relaxkit`.

[Unreleased]: https://github.com/cpenaforte/relaxkit/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/cpenaforte/relaxkit/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/cpenaforte/relaxkit/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/cpenaforte/relaxkit/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cpenaforte/relaxkit/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cpenaforte/relaxkit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cpenaforte/relaxkit/releases/tag/v0.1.0
