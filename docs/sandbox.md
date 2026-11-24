# relaxkit sandbox

This page explains the sandbox that `relaxkit setup` turns on for Claude Code and OpenCode, and which relaxkit commands run outside it. The [command reference](commands.md) has the syntax of each command.

## Claude Code

`relaxkit setup` turns on Claude Code's sandbox for shell commands. It sets these keys in Claude Code's settings file, `~/.claude/settings.json`, and keeps every other key, except `sandbox.enableWeakerNetworkIsolation` when its value is `true` (see [macOS](#macos) below):

- `sandbox.enabled` is `true`.
- `sandbox.autoAllowBashIfSandboxed` is `true`, so sandboxed commands run without asking.
- `sandbox.failIfUnavailable` is `true`, so a command fails when the sandbox cannot start, rather than running without it.

Network access is limited to a short list. relaxkit adds these domains to `sandbox.network.allowedDomains`, and keeps the domains you already listed:

- `github.com`
- `api.github.com`
- `registry.npmjs.org`
- `pypi.org`
- `files.pythonhosted.org`
- `crates.io`
- `static.crates.io`

### Commands that run outside the sandbox

The commands that change the machine run outside the sandbox, because they change the machine. They are listed in `sandbox.excludedCommands`: `setup`, `update`, `config`, `preset`, `privacy`, `login`, `undo`, `uninstall` and `doctor`. Each one is excluded under both `relaxkit` and `rlx`. The `rlx` alias is added by the installer when that name is free. `status` and `usage` stay inside the sandbox.

`gh` also runs outside the sandbox, as a single plain invocation: relaxkit adds `gh *` to `sandbox.excludedCommands`. The v0.1.3 README documents that a `gh` call inside a pipe, a chain or after a `cd` stays sandboxed and cannot authenticate. The v0.1.3 README says that is Claude Code's behaviour; this repository does not test it.

### macOS

On macOS, a route that runs `gh` inside the sandbox needs `sandbox.enableWeakerNetworkIsolation`. That setting opens a possible data path through the macOS trust service. This version does not use that route, so `relaxkit setup` does not set the key. `relaxkit setup` removes `sandbox.enableWeakerNetworkIsolation` whenever it is `true`, including a value you set yourself.

### Linux

On Linux, the sandbox needs bubblewrap and socat. When either package is missing, or the AppArmor profile is missing on Ubuntu 24.04 and later, `relaxkit setup` prints the one sudo command that does the missing work and asks you to run it once. It does not run that command unless you pass `--allow-sudo`:

```sh
relaxkit setup --allow-sudo
```

With `--allow-sudo`, the one sudo run installs both packages with `apt-get`. On Ubuntu 24.04 and later, it also writes an AppArmor profile at `/etc/apparmor.d/relaxkit-bwrap`, so that bubblewrap may create user namespaces, and loads that profile. When both packages are on the PATH, and the AppArmor profile is already in place where Ubuntu needs it, nothing runs.

## OpenCode

`relaxkit setup` installs relaxkit's own sandbox plugin, `relaxkit-opencode`, into OpenCode. The plugin comes from relaxkit's GitHub repository at the release tag that matches the binary, so `setup` needs access to GitHub on an OpenCode host. If the plugin cannot be installed, `setup` stops.

The plugin is a fork of `opencode-sandbox` 0.7.0, which is MIT licensed, built on `@anthropic-ai/sandbox-runtime` 0.0.83. Its settings are fixed:

- It runs in enforce mode. When the sandbox cannot wrap a command, the command is blocked with the message `Sandbox unavailable in enforce mode; command blocked`.
- A project file cannot change the settings, and no `OPENCODE_SANDBOX_*` or `OPENCODE_DISABLE_SANDBOX` variable does. The settings are built in code, from the locations of the OpenCode config directory and the temporary directory.
- Reads are denied under your home directory for `.ssh`, `.gnupg`, `.aws/credentials`, `.azure`, `.config/gcloud`, `.config/gh`, `.kube`, `.docker/config.json`, `.npmrc`, `.netrc` and `.env`.
- Writes are allowed in the working directory, in the project root and in the temporary directory.
- Writes are denied to `.opencode`, `opencode.json`, `opencode.jsonc` and the OpenCode config directory, which is `~/.config/opencode` by default.
- Network access is limited to the same seven domains as Claude Code. Local binding is off.

On OpenCode, `gh` runs only through a `gh` tool that the plugin provides. The tool accepts only the built-in `gh` command names that the plugin lists. Any other first argument is refused, which covers the aliases and extensions you installed in `gh`, and also `alias`, `auth`, `config`, `extension` and `help`. Each call also passes OpenCode's permission check. The plugin's tool description says that a `gh` command typed into OpenCode's shell tool runs inside the sandbox and cannot reach GitHub, and tells you to use the `gh` tool instead. This repository does not test that claim.

The plugin's rules are in `adapters/opencode-plugin/src/config.js` and `adapters/opencode-plugin/src/gh-tool.js`.

## Self-test

`relaxkit doctor` runs a sandbox self-test for each host it finds. It does not run in a dry run. Three probes check that the sandbox refuses each of these:

- a write outside the project,
- a direct connection that bypasses the proxy, to `api.github.com`,
- a host that is not on the list, `example.com`.
