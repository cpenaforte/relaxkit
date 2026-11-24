# relaxkit configuration

This page explains the settings that `relaxkit config` and `relaxkit preset` write: the roles, the tiers, the presets, the voice and the compaction threshold. It also lists the files that relaxkit writes. The full command syntax is in the [command reference](commands.md#relaxkit-config).

## Roles

A role is one step of the work, and each role runs on a model. A change without `--host` covers both hosts.

| Role | Runs for | Claude Code | OpenCode |
| --- | --- | --- | --- |
| `planner` | Planning, in the main session | `planner` line in the roles block of CLAUDE.md | The `plan` agent |
| `implementer` | Each task of a plan | `implementer` line in the roles block of CLAUDE.md | The `implementer` agent |
| `reviewer` | Review of one task's changes | `reviewer` line in the roles block of CLAUDE.md | The `reviewer` agent |
| `escalation` | Fix-loop rounds 4 and 5 | `escalation` line in the roles block of CLAUDE.md | The `implementer-escalated` agent |
| `final-review` | Review of the whole branch before merge | `final-review` line in the roles block of CLAUDE.md | The `reviewer-final` agent |

On Claude Code, the model of each role is written as text in the roles block of `~/.claude/CLAUDE.md`, and `relaxkit status` marks it as advisory. On OpenCode, the setting sets the model of the agent in the table. Sources: `src/config_apply/claude.rs` (`roles_block_text`) and `src/config_apply/opencode.rs` (`SUBAGENTS`, `PLANNER_AGENT`).

A role without a setting of its own runs at the tier of the preset. `escalation` and `final-review` run one tier above the implementer and the reviewer, and never above `smarter`. A pinned role counts as the tier of its model family when those two are derived: a model name with `haiku` is `cheap`, one with `opus` is `smarter`, and any other model is `standard`. Source: `src/config.rs` (`effective`) and `src/roles.rs` (`tier_of`).

## Tiers and exact models

A role takes either a tier or an exact model.

- The tiers are `cheap`, `standard` and `smarter`, where `smarter` is the strongest. On Claude Code they map to `haiku`, `sonnet` and `opus`.
- On OpenCode, a tier is the newest Anthropic model of its family in the list that `opencode models --standalone` prints. The tier needs that list. When the list cannot be read, `config set` and `config unset` refuse a tier for OpenCode and ask for an exact model.
- An exact model takes the form `provider/model#variant`, where the `#variant` part is optional, such as `anthropic/claude-sonnet-5-5#high`. A bare `claude-*` id, or one of `haiku`, `sonnet`, `opus` and `fable`, means Anthropic.
- On Claude Code the `#variant` is ignored. A pin to another provider is not used by Claude Code, so that role keeps its preset tier there.
- On OpenCode, an exact model must be in the list, or the command refuses it. When the list cannot be read, the exact model is accepted with a warning.

Sources: `src/roles.rs` (`Tier`, `ModelSpec::parse`, `resolve_claude`, `resolve_opencode`) and `src/commands/config.rs` (`refuse_tier_without_list`).

## Presets

| Preset | Planner | Implementer | Reviewer | Escalation (derived) | Final review (derived) | Voice |
| --- | --- | --- | --- | --- | --- | --- |
| `lean` | standard | cheap | standard | standard | smarter | ultracave |
| `balanced` | smarter | cheap | standard | standard | smarter | caveman |
| `careful` | smarter | standard | smarter | smarter | smarter | off |

The table gives each preset's tiers and voice. Escalation and final review follow the rule in [Roles](#roles). Source: `src/presets.rs` (`tiers`, `voice`).

On a machine with no config, `relaxkit setup` writes the `balanced` preset. `relaxkit preset lean`, `relaxkit preset balanced` or `relaxkit preset careful` applies a preset's tiers and voice on each detected host. Roles pinned to an exact model keep their pins, and a voice set by hand is cleared. See the [preset command](commands.md#relaxkit-preset).

## Voice

`voice` takes `caveman`, `ultracave`, `megacave` or `off`. relaxkit writes the value as `defaultMode` in caveman's config file, `~/.config/caveman/config.json`, and keeps the other keys in that file. With `--project`, it writes `.caveman.json` at the project root instead. The voice applies to both hosts, so `--host` is refused. `relaxkit config unset voice` restores the voice of the preset. Source: `src/config_apply/voice.rs`.

## Compaction threshold

`compact-at` takes a token count. It is a whole number with an optional `k` or `m` suffix, in either case: `256k` is 256000, `1m` is 1000000, and `256000` is itself. Zero is refused, and so is a count above the largest TOML integer. The value is stored as `compact_at` in `config.toml`. Source: `src/config.rs` (`parse_compact`).

- On Claude Code, the threshold is written as `autoCompactWindow` under `modelSettings`, once for each Claude model that the roles resolve to, in `~/.claude/settings.json`.
- On OpenCode, relaxkit writes nothing today. It has no verified window size for OpenCode's default model, so OpenCode keeps its own threshold, and the command prints a warning. Source: `src/commands/config.rs` (`set_compact`) and `src/config_apply/opencode.rs` (`apply_compact`).
- `relaxkit config unset compact-at` removes the keys that relaxkit wrote on both hosts, and the stored `compact_at`.

## The settings file

relaxkit stores its settings in `~/.config/relaxkit/config.toml`. On a machine with no config yet, the file holds the lines below after these three commands.

```sh
relaxkit config set implementer cheap --host claude
relaxkit config set compact-at 256k
relaxkit config set voice caveman
```

```toml
preset = "balanced"
compact_at = 256000
voice = "caveman"

[hosts.claude.roles]
implementer = "cheap"
```

A role set without `--host` is stored under `[roles]`. A role set with `--host` is stored under `[hosts.<host>.roles]`, and it takes priority over `[roles]` on that host. A key that is not in the file takes the preset's value.

## Changing settings

```sh
relaxkit config show
relaxkit config set implementer cheap --host claude
relaxkit config set reviewer smarter --host opencode
relaxkit config set planner anthropic/claude-sonnet-5-5#high
relaxkit config unset planner --host claude
relaxkit config set compact-at 256k
relaxkit config set voice caveman
```

- `--host claude` or `--host opencode` limits a role change to one host. `--host` applies to roles only: `voice` and `compact-at` refuse it, because they apply to both hosts.
- `--project` applies a change to the current project. For `voice`, it writes the project's `.caveman.json`. A `config` change with `--project` leaves `config.toml` alone.
- A role change takes effect in new sessions.
- Every change is journaled, so `relaxkit undo` reverts it. See the [undo command](commands.md#relaxkit-undo).

## Files relaxkit writes

The tables list the files that relaxkit writes, and the files that the Claude Code and OpenCode commands write when relaxkit runs them. The paths are the defaults for macOS, where the runs were made in a disposable HOME. The relaxkit paths are the same on Linux, because `src/platform.rs` has no operating-system branch for them. The rtk and Playwright paths differ, and the table gives both.

A row marked **observed** was seen in a dry or a real run in a disposable HOME. A row marked **source** was not run: its path comes from the file named in the row.

### relaxkit's own files

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.config/relaxkit/config.toml` | The settings on this page. | Observed |
| `~/.config/relaxkit/lock.toml` | The exact version and route of each installed tool, per host. Written by `setup` and `update`, and restored by `undo` to its state before the run. Source: `src/state/lockfile.rs`, `src/commands/undo.rs`. | Source |
| `~/.local/share/relaxkit/journal.jsonl` | One record for each file that a run changed, read by `relaxkit undo`. | Observed |
| `~/.local/share/relaxkit/backups/` | A folder per run, with a copy of each file as it was before the run changed it. Source: `src/state/journal.rs`, `src/commands/undo.rs`. | Observed |
| `~/.local/share/relaxkit/marketplaces.jsonl` | The Claude Code marketplaces that a run added. `undo` names each one and the command that removes it, because it does not remove it. Source: `src/state/journal.rs`, `src/commands/undo.rs`. | Source |
| `~/.local/share/relaxkit/journal.lock` and `run.lock` | Empty lock files, held while a run writes. | Observed |
| `~/.local/share/relaxkit/bin/` | Binaries that relaxkit links or installs for itself, such as `rtk` and `uv`. Source: `src/runtime/rtk.rs`, `src/runtime/uv.rs`. | Source |
| `~/.local/share/relaxkit/node/` | The Node that the npm installs use. Source: `src/tools/handlers.rs`. | Source |

`relaxkit undo` does not restore the host's own registry state. That is the Claude Code plugin registry, the caveman state and the Serena state. The undo report names each plugin version that moved and each marketplace that a run added. Source: `src/commands/undo.rs`.

### Claude Code

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.claude/settings.json` | The `modelSettings` compaction windows, the sandbox keys written by `setup`, and `env.SUPERPOWERS_DISABLE_TELEMETRY`. The compaction windows are observed. The sandbox keys are source: `src/sandbox/claude.rs`. The telemetry key is source: `src/config_apply/claude.rs`. | Observed in part |
| `~/.claude/CLAUDE.md` | The roles block, between the markers `<!-- relaxkit:begin roles -->` and `<!-- relaxkit:end roles -->`. | Observed |
| `~/.claude.json` | Claude Code's state file, which `claude mcp` and `claude plugin` write. Source: `src/host/claude.rs`. | Source |
| `~/.claude/plugins/known_marketplaces.json` and `installed_plugins.json` | Claude Code's plugin registry, which `claude plugin` writes. Source: `src/host/claude.rs`. | Source |

The Claude Code files are in `~/.claude` by default.

### OpenCode

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.config/opencode/opencode.json`, or `opencode.jsonc` when that file exists | The model of each relaxkit agent, and the plugin list. Source: `src/host/opencode.rs`. | Observed for `opencode.json` |
| `~/.config/opencode/AGENTS.md` | The routing block. | Observed |

The OpenCode files are in `~/.config/opencode` by default.

### Caveman and rtk

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.config/caveman/config.json` | The voice, as `defaultMode`. | Observed |
| `~/Library/Application Support/rtk/config.toml` on macOS, `~/.config/rtk/config.toml` on Linux | rtk's config. relaxkit sets `[display] pro_hint = false`, which hides rtk's Pro notice. The Linux path is marked in `src/platform.rs` as still to verify. Source: `src/platform.rs`, `src/runtime/rtk.rs`. | Source |

### Playwright

| Path | What it holds | Status |
| --- | --- | --- |
| `~/Library/Caches/ms-playwright` on macOS, `~/.cache/ms-playwright` on Linux | Where Playwright keeps Chromium. relaxkit looks there for the browser. Source: `src/platform.rs`, `src/runtime/chromium.rs`. | Source |
