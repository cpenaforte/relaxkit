# Changelog

All notable changes to relaxkit are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Release pipeline (`.github/workflows/release.yml`): a `v*` tag builds the four release targets
  (macOS on arm64 and x86_64, Linux on x86_64 and arm64, musl), packages each as
  `relaxkit-<version>-<target>.tar.gz`, writes the checksums file, signs it with minisign, verifies the
  signature against the committed public key, and only then publishes the GitHub release.
- Homebrew formula template (`packaging/homebrew/relaxkit.rb.tmpl`) and renderer
  (`scripts/render-formula.sh`), which fills in the four asset URLs and checksums of one release. The
  formula installs `rlx` as a symlink to `relaxkit`.
