# Changelog

All notable changes to relaxkit are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] - 2026-11-23

### Changed

- The sandbox self-test failure message now tells you to log in to Claude Code first. The probes run
  Claude Code, which does not answer until it is logged in, and that is the usual cause on a new machine.

### Fixed

- On Linux arm64, relaxkit finds Chromium in the `chrome-linux-arm64` folder that Playwright unpacks it
  to, so `relaxkit setup` no longer stops at the browser step on a fresh arm64 Linux install.

## [0.1.2] - 2026-11-23

### Changed

- Tool recipes name no version. An install takes the newest release the registry reports, and a repair
  with no lock entry does the same. Tools that are already present are recognized without a registry
  lookup, so that check needs no network.
- Runtimes pinned by commit SHA (nvm v0.41.0, uv 0.13.9 and Node major 24) are the exception and stay
  at those versions.
- `relaxkit undo` names each Claude marketplace that a run added, with the command that removes it. It
  does not remove the marketplace itself.

### Fixed

- `relaxkit update` compares the version each host reports, not the lock entry, so a host behind the
  locked version is no longer shown as current.
- An npm tool on OpenCode reports the version its package has in the node tree. When no version can be
  read, the tool is adopted as unversioned and is not written to the lock.
- Removing a managed block from a file with no final newline leaves the rest of the file byte-exact.

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
  `{version}`, instead of printing "not run here". Offline, a probe now fails where it used to warn.

## [0.1.0] - 2026-11-22

### Added

- Every command prints its results with the same marks: `✓` done, `!` done with something to check,
  `✗` failed, and `·` not done. A dry run prints only `·` plans, so no dry-run line claims a change.
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
- Homebrew formula template (`packaging/homebrew/relaxkit.rb.tmpl`).

### Fixed

- The install script runs under dash on a machine with no terminal.

[Unreleased]: https://github.com/cpenaforte/relaxkit/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/cpenaforte/relaxkit/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cpenaforte/relaxkit/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cpenaforte/relaxkit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cpenaforte/relaxkit/releases/tag/v0.1.0
