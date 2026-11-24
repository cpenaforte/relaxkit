# Troubleshooting relaxkit

Each entry gives the message you see, its cause and the fix. The messages are copied from relaxkit's output or from its source. Where a message contains a path or a command, the text shows it as it is printed. Some outcomes are not errors: an exit code 3 or a pending step means setup is waiting for you, and the entry says so.

For the exit codes, see [exit codes](commands.md#exit-codes). For the first run, see [getting started](getting-started.md).

## Neither host was found

```text
Error: neither Claude Code nor OpenCode was found on the PATH; install one of them first, then run `relaxkit setup` again
```

Setup looks for Claude Code and OpenCode only on your `PATH`, and stops when it finds neither (`src/commands/setup.rs` lines 70 to 75).

Install Claude Code or OpenCode, make sure its directory is on your `PATH`, then run:

```sh
relaxkit setup
```

## The sandbox self-test fails

```text
Claude Code: the sandbox did not refuse: <probe>: <detail>
fix  if Claude Code is not logged in, run `claude` and then `/login`; then run `relaxkit setup` again. Until every probe passes, the sandbox is not enforcing
```

The self-test runs Claude Code, and Claude Code does not answer until you have logged in. The fix line is the text from `src/commands/doctor.rs` line 248. The message reads `could not run` instead of `did not refuse` when a probe did not run at all (`src/commands/doctor.rs` lines 272 to 282).

Run `claude`, enter `/login` inside Claude Code, then run:

```sh
claude
relaxkit setup
```

To see the checks again without running setup, run `relaxkit doctor`.

## Linux packages are missing

Setup prints this line when bubblewrap or socat is missing on Linux:

```text
Linux packages are missing; run this once yourself, then run setup again: sudo sh -c '...'
```

`relaxkit doctor` reports the same missing packages:

```text
the sandbox needs bwrap and socat on the PATH, and it is missing
fix  run this once yourself (relaxkit does not run sudo without --allow-sudo): sudo sh -c '...'
```

The sandbox needs both packages. Setup runs the sudo command only when you pass `--allow-sudo` (`src/commands/setup.rs` lines 320 to 327, and `src/commands/doctor.rs` lines 152 to 162). On Ubuntu 24.04 and later, the same sudo command also writes the AppArmor profile `/etc/apparmor.d/relaxkit-bwrap` and loads it, so that bubblewrap may create user namespaces. Doctor names that profile in its message when the profile is missing (`src/commands/doctor.rs` lines 141 to 143, and `src/platform.rs` lines 166 to 175).

Run the sudo command that setup printed, once, yourself, then run setup again. Or let relaxkit run it:

```sh
relaxkit setup --allow-sudo
```

Until the packages are in place, setup exits 1, so the installer also reports a setup failure. See [the installer exits 1](#the-installer-exits-1-when-setup-has-a-manual-step).

## Exit code 3: the GitHub login needs a terminal

```text
GitHub login needs a terminal. Run this in one to finish: <command>
finish the login yourself, then run setup again: <command>
```

The first line comes from `login github` and the second from `setup`. The `<command>` is the gh login followed by `gh auth setup-git`, with the full path to gh (`src/github.rs` lines 73 to 79, and `src/commands/setup.rs` lines 363 to 365).

Login needs a terminal for its browser flow. Without one, relaxkit prints the command and exits 3 (`src/github.rs` lines 34 to 38, and `src/commands/setup.rs` lines 194 to 198). Exit code 3 means a manual step remains; it is not a failure.

Run the login in a terminal, then run setup again:

```sh
relaxkit login github
relaxkit setup
```

## The installer exits 1 when setup has a manual step

```text
install.sh: relaxkit setup failed; the binary is installed at $HOME/.local/bin/relaxkit, so run relaxkit setup again
```

This is a known outcome, not a download or checksum error. The installer has already written the binary when it runs setup. It then exits 1 whenever `relaxkit setup` exits with any non-zero code (`scripts/install.sh` lines 164 and 165, and the `die` function at lines 33 to 36).

Setup exits non-zero in two cases. In the first, the GitHub login needs a terminal and setup exits 3 (see [exit code 3](#exit-code-3-the-github-login-needs-a-terminal)). In the second, a check in setup's final verify step fails and setup exits 1. A missing Linux package is one such failed check (see [Linux packages are missing](#linux-packages-are-missing)), so on a Linux machine without bubblewrap and socat, the installer exits 1 even when the login is done (`src/commands/setup.rs` lines 189 to 198, and `src/commands/doctor.rs` lines 123 to 163).

The installer does not run setup again on its own. Finish the step that setup named, then run:

```sh
relaxkit setup
```

When the installer runs in a terminal, it gives setup the terminal as its input (`scripts/install.sh` lines 160 to 163), so the login can open its browser flow there. The installer's exit status above is the current behaviour of this repository, not a promise of a change.

## The rlx alias was not created

```text
note: $HOME/.local/bin/rlx already exists and is not a link to relaxkit, so it was left alone. Use the name relaxkit.
```

The installer adds `rlx` only when that name is free. It never replaces an existing file or a link that does not point to relaxkit (`scripts/install.sh` lines 146 to 155). `relaxkit` runs the same binary, so you can use that name.

To get `rlx` back, move the existing file out of the way yourself, then run the install line again:

```sh
curl -fsSL https://raw.githubusercontent.com/cpenaforte/relaxkit/main/scripts/install.sh | sh
```

## Linux arm64: no known executable path for Chromium

```text
the Chromium under <dir> has no known executable path for this OS
```

Relaxkit versions before 0.1.3 looked for Chromium only in folders that did not match the `chrome-linux-arm64` folder that Playwright unpacks on Linux arm64. Setup stopped at the browser step with this message. Version 0.1.3 fixed it (`CHANGELOG.md`, section 0.1.3, Fixed; `src/runtime/chromium.rs` lines 147 to 150).

Update relaxkit, then run setup again:

```sh
relaxkit update --self
relaxkit setup
```

You can also run the install line again, which installs the latest release.

## OpenCode's model list could not be read

```text
Error: OpenCode's model list could not be read, so a tier cannot be applied to OpenCode now: pin an exact model (provider/model) or retry when `opencode models --standalone` works
```

Relaxkit reads OpenCode's model list with `opencode models --standalone`. When that fails, a tier such as `cheap` cannot be mapped to a model for OpenCode, so the change is refused (`src/commands/config.rs` line 231). During setup, the same condition prints this warning instead, and the tier roles keep their models until the next setup run (`src/commands/setup.rs` lines 251 to 264):

```text
OpenCode's model list could not be read, so the tier roles were not applied; their models stay as they are until the next setup run
```

Change Claude Code only, or retry when `opencode models --standalone` prints its list:

```sh
relaxkit config set implementer cheap --host claude
opencode models --standalone
```

To pin OpenCode to an exact model instead, set the role to `provider/model`, with `--host opencode`. The forms are in [configuration.md](configuration.md#tiers-and-exact-models).
