# Contributing to relaxkit

relaxkit installs, configures, updates and diagnoses a curated set of agent tools for Claude Code and OpenCode. Most changes touch the Rust CLI, the recipe files that describe each tool, or the documentation.

## Ways to contribute

- **Report a bug:** open an issue with the bug report form. Before you do, read [troubleshooting](docs/troubleshooting.md), which lists known failures and what to do about each one.
- **Suggest a feature:** open an issue with the feature request form.
- **Report a security problem:** do not open a public issue. Follow [SECURITY.md](SECURITY.md).
- **Change code or documentation:** open a pull request. See [Pull requests](#pull-requests) below.

## Development setup

Install Rust 1.96, the toolchain that CI uses, with [rustup](https://rustup.rs). Then build the project:

```sh
cargo build
```

relaxkit supports macOS and Linux. The README states the platform scope.

`tests/release_pipeline.rs` runs `ruby -c` on the generated Homebrew formula when `ruby` is installed. One test in the same file needs `ruby` to read `.github/workflows/release.yml`, so install `ruby` before you run the full suite.

## Checks

Run these three commands before you open a pull request. They are the commands that `.github/workflows/ci.yml` runs on Ubuntu and macOS:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
```

`.github/workflows/e2e-linux.yml` also runs `cargo test --test e2e_linux` on Linux. That test runs a dry run, setup and `doctor --json` in a scratch home, with stub host commands and the network closed. Run it as well when you change setup, the dry run or the doctor checks.

## Layout

```text
src/        the Rust CLI and library
recipes/    one TOML file per tool
adapters/   the Claude Code and OpenCode plugins
scripts/    the installer and the Homebrew formula renderer
packaging/  the Homebrew formula template
tests/      integration tests, fixtures and test support
```

The binary-level tests run `relaxkit` with a temporary HOME and a PATH of stub commands, both set up in `tests/support/mod.rs`. They do not use your real home directory or your real PATH.

## Adding or changing a tool recipe

Recipes name no tool version. An install takes the newest release that the registry reports. The only other version numbers in a recipe are its minimum host versions (Claude Code 2.1.330 and OpenCode 2.2.6, in the `[minimum_host]` table, for example in `recipes/ccusage.toml`), and the serena recipe also pins Python 3.13 with `-p`. `tests/recipes_conformance.rs` checks that each tool, on each host, expands to the exact argument list it should.

## Documentation

`tests/docs_contract.rs` checks the public Markdown files. It checks that the README matches the real command-line interface, that every relative link resolves, that the `sh` examples that start with `relaxkit` or `rlx` parse, and that the files carry no personal data.

Use `sh` fences for commands you can run. Use `text` fences for synopses and for output. Do not put placeholders in `sh` fences.

## Pull requests

- Keep each pull request to one logical change. Add or update tests for any change in behaviour.
- Add a line under `## [Unreleased]` in `CHANGELOG.md` for each change a user can see. Use one of the Keep a Changelog sections: Added, Changed, Deprecated, Removed, Fixed or Security. A change that users cannot see does not need an entry.
- Update the documentation and `tests/docs_contract.rs` when a change affects behaviour that a document describes.
- Write code, comments, output, documentation and tests in English.
- Run the three commands in [Checks](#checks). The pull request template lists them.

## Releasing

This section is for maintainers. A release is a tag `vX.Y.Z` on `main`, where `X.Y.Z` is the version. Pushing the tag starts `.github/workflows/release.yml`.

1. Set the new version in these files: `Cargo.toml`, the `relaxkit` entry in `Cargo.lock`, `adapters/claude-plugin/.claude-plugin/plugin.json`, `adapters/opencode-plugin/package.json`.
2. Find any other references to the old version with `git grep -nF "<current version>" -- . ':!Cargo.lock' ':!*package-lock.json'`, where `<current version>` is the old version. At 0.1.3 the search also finds the user-agent assertion in `src/net.rs`. The user agent is built from the crate version (`src/net.rs`, the `user_agent` call), so update that assertion to the new version. Lines in `CHANGELOG.md` and in `docs/` that describe an earlier release stay as they are. Example output that shows the current version, such as `relaxkit 0.1.3` in `docs/getting-started.md`, changes to the new version.
3. In `CHANGELOG.md`, move the entries under `## [Unreleased]` into a new `## [X.Y.Z] - YYYY-MM-DD` section directly below it, and leave `## [Unreleased]` empty. Then update the `[Unreleased]` and `[X.Y.Z]` compare links at the bottom of the file.
4. Run the checks in [Checks](#checks), then commit the release on `main`.
5. Tag the commit `vX.Y.Z` and push the tag. The release workflow then:
   - stops if the tag does not match the `version` in `Cargo.toml`;
   - builds the four release targets, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, and packages each one as `relaxkit-X.Y.Z-<target>.tar.gz`;
   - writes the checksums file and signs it with minisign, using the secret `MINISIGN_KEY`. The key has no password, because the job cannot prompt. The workflow writes the key only to a temporary file under `RUNNER_TEMP` and removes that file after signing;
   - verifies the signature against `release/minisign.pub` and checks every archive against the checksums;
   - renders the Homebrew formula and publishes the GitHub release with the archives, the checksums file, its signature and the formula.

Report security problems in a release through [SECURITY.md](SECURITY.md).
