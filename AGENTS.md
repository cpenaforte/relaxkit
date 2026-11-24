# AGENTS.md

Instructions for AI coding agents that work in this repository. Read this file first. The rules here follow [CONTRIBUTING.md](CONTRIBUTING.md), which is the full contributor guide.

## What the repository is

A curated pack of agent tools for Claude Code and OpenCode: install, configure, update and diagnose from one CLI.

The crate is `relaxkit`, a Rust library and binary. The binary has an optional short alias, `rlx`. relaxkit supports macOS and Linux.

## Commands

Install Rust 1.96, the toolchain that CI uses. Run these from the repository root. CI runs them on Ubuntu and macOS (`.github/workflows/ci.yml`):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
```

One test in `tests/release_pipeline.rs` needs `ruby`, so install `ruby` before you run the full suite.

For a documentation-only change, run the documentation contract test:

```sh
cargo test --test docs_contract
```

Run the end-to-end test too when you change setup, the dry run or the doctor checks. The `e2e-linux` CI job runs it on Ubuntu (`.github/workflows/e2e-linux.yml`):

```sh
cargo test --test e2e_linux
```

## Layout

```text
src/        the Rust CLI and library
recipes/    one TOML file per tool
adapters/   the Claude Code and OpenCode plugins
scripts/    the installer and the Homebrew formula renderer
packaging/  the Homebrew formula template
release/    the minisign public key that verifies release signatures
docs/       the public documentation pages
tests/      integration tests, fixtures and test support
```

## Conventions

- Write code, comments, output, documentation and tests in English.
- Recipes name no tool version. An install takes the newest release that the registry reports. The exceptions are the minimum host versions in each recipe's `[minimum_host]` table and the Python pin in `recipes/serena.toml`.
- Binary-level tests run `relaxkit` with a temporary HOME and a fake PATH of stub commands, both set up in `tests/support/mod.rs`. They never touch the real home directory or the real PATH.
- Tests do not call the network. The binary-level tests point the proxy variables at a closed local port, and the HTTP test in `src/net.rs` serves from 127.0.0.1.
- Keep each pull request to one logical change. Add or update tests for any change in behaviour.

## Commits and pull requests

- Make changes on a branch and open a pull request. Do not push to main.
- Commit with the maintainer's public GitHub noreply identity, never with a personal email address.
- A commit that an agent wrote ends with a `Co-Authored-By:` trailer that names the model. Do not add session links or session identifiers to commit messages or pull request descriptions.
- For each change that a user can see, add a line under `## [Unreleased]` in `CHANGELOG.md`, in one of the Keep a Changelog sections: Added, Changed, Deprecated, Removed, Fixed or Security.
- Update the documentation, and `tests/docs_contract.rs`, when a change affects behaviour that a document describes.
- Run the three checks above before you open the pull request. The pull request template asks for them.

## Documentation

- `tests/docs_contract.rs` checks the public Markdown files. It checks that the README matches the real command-line interface, that every relative link resolves, that each `sh` example starting with `relaxkit` or `rlx` parses, and that no public file carries personal data.
- Use `sh` fences only for commands you can run, and put no placeholders in them. Use `text` fences for synopses and output.
- Public files do not cite private planning documents.

## Releases

Releases are a maintainer task. Follow the [Releasing](CONTRIBUTING.md#releasing) section of CONTRIBUTING.md. Do not tag, publish or edit a release as part of another change.
