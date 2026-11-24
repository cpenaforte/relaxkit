# Security policy

## Supported versions

Only the latest release of relaxkit receives security fixes. To move to it, run `relaxkit update`.

## Reporting a vulnerability

Report a vulnerability privately through GitHub's private vulnerability reporting: <https://github.com/cpenaforte/relaxkit/security/advisories/new>

Do not open a public issue or pull request for a vulnerability. Include:

- the output of `relaxkit --version`, and your operating system and CPU;
- the steps to reproduce it, with the exact command you ran;
- what you expected to happen and what happened instead;
- the impact you see. Replace your home directory path with `~` in any output you paste.

The first response to a report comes within 14 days.

## Scope

In scope:

- the installer script, `scripts/install.sh`;
- the release archives, their checksums file and their minisign signatures;
- the configuration that relaxkit writes for Claude Code and OpenCode;
- the sandbox settings that relaxkit applies.

A vulnerability in a tool that relaxkit installs belongs to that tool, and you should report it to that tool's maintainers. The exception is a case where relaxkit's own logic makes the problem worse, for example by passing input to the tool in a way that makes the flaw reachable. Report that case to relaxkit.

## Verifying a release

The installer checks the archive's SHA-256 against the release's checksums file. It does not check the minisign signature. To check the signature yourself, run the commands below from the root of a clone of this repository, checked out at the `v0.1.3` tag (`git switch --detach v0.1.3`), so that `release/minisign.pub` resolves. That file is the public key that the binary embeds in `src/selfupdate.rs`. The commands download the v0.1.3 assets into `dist/`.

```sh
mkdir -p dist
curl -fsSLo dist/relaxkit-0.1.3-checksums.txt https://github.com/cpenaforte/relaxkit/releases/download/v0.1.3/relaxkit-0.1.3-checksums.txt
curl -fsSLo dist/relaxkit-0.1.3-checksums.txt.minisig https://github.com/cpenaforte/relaxkit/releases/download/v0.1.3/relaxkit-0.1.3-checksums.txt.minisig
curl -fsSLo dist/relaxkit-0.1.3-aarch64-apple-darwin.tar.gz https://github.com/cpenaforte/relaxkit/releases/download/v0.1.3/relaxkit-0.1.3-aarch64-apple-darwin.tar.gz
minisign -Vm dist/relaxkit-0.1.3-checksums.txt -p release/minisign.pub
(cd dist && shasum -a 256 -c --ignore-missing relaxkit-0.1.3-checksums.txt)
```

The example checks the Apple silicon archive. For another platform, download its archive instead; the file names are listed in the checksums file. On Linux, use `sha256sum -c --ignore-missing` in place of `shasum -a 256 -c --ignore-missing`.

A good run prints:

```text
Signature and comment signature verified
Trusted comment: relaxkit 0.1.3
relaxkit-0.1.3-aarch64-apple-darwin.tar.gz: OK
```

The first line shows that the checksums file was signed with the key in `release/minisign.pub`. The last line shows that the archive matches that checksums file.
