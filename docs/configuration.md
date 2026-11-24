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

A role without a setting of its own runs at the tier of the preset, except `escalation` and `final-review`. Those two are derived from the implementer and the reviewer: each runs one tier above the tier of its source role, and never above `smarter`. A setting of its own for `escalation` or `final-review` replaces the derivation. A pinned role counts as the tier of its model family when it is the source of a derivation: a model name with `haiku` is `cheap`, one with `opus` is `smarter`, and any other model is `standard`. Source: `src/config.rs` (`effective`, lines 123-139) and `src/roles.rs` (`tier_of`, lines 166-181).

## Tiers and exact models

A role takes either a tier or an exact model.

- The tiers are `cheap`, `standard` and `smarter`, where `smarter` is the strongest. On Claude Code they map to `haiku`, `sonnet` and `opus`.
- On OpenCode, a tier is the newest Anthropic model of its family in the list that `opencode models --standalone` prints. The tier needs that list. When the list cannot be read, `config set`, `config unset` and `preset` refuse a tier for OpenCode and ask for an exact model instead. Source: `src/commands/config.rs` (`refuse_tier_without_list`) and `src/commands/preset.rs` (lines 24-30).
- An exact model takes the form `provider/model#variant`, where the `#variant` part is optional, such as `anthropic/claude-sonnet-5-5#high`. A bare `claude-*` id, or one of `haiku`, `sonnet`, `opus` and `fable`, means Anthropic.
- On Claude Code the `#variant` is ignored. A pin to another provider is not used by Claude Code, so that role keeps its preset tier there.
- On OpenCode, an exact model must be in the list, or the command refuses it. This applies only when OpenCode is installed. When OpenCode is not installed, or the list cannot be read, the exact model is accepted with a warning.

Sources: `src/roles.rs` (`Tier`, `ModelSpec::parse`, `resolve_claude`, `resolve_opencode`) and `src/commands/config.rs` (`check_pin`, lines 439-468).

## Presets

| Preset | Planner | Implementer | Reviewer | Escalation (derived) | Final review (derived) | Voice |
| --- | --- | --- | --- | --- | --- | --- |
| `lean` | standard | cheap | standard | standard | smarter | ultracave |
| `balanced` | smarter | cheap | standard | standard | smarter | caveman |
| `careful` | smarter | standard | smarter | smarter | smarter | off |

The table gives each preset's tiers and voice. Escalation and final review follow the rule in [Roles](#roles). Source: `src/presets.rs` (`tiers`, `voice`).

On a machine with no config, `relaxkit setup` writes the `balanced` preset. `relaxkit preset lean`, `relaxkit preset balanced` or `relaxkit preset careful` applies a preset's tiers and voice on each detected host. Roles pinned to an exact model keep their pins, and a voice set by hand is cleared. When OpenCode's model list cannot be read, a preset is refused for the tiers it would set on OpenCode, as `config set` is. See the [preset command](commands.md#relaxkit-preset).

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
- The exact-model example above, `anthropic/claude-sonnet-5-5#high`, depends on OpenCode's list. With OpenCode installed and its list readable, the command refuses the pin when the list does not contain `anthropic/claude-sonnet-5-5`. Without OpenCode, or when the list cannot be read, it is accepted with a warning. Source: `src/commands/config.rs` (`check_pin`, lines 439-468).
- Every change is journaled as part of a run group. `relaxkit undo` reverts the newest run group. An older change needs `relaxkit undo --run <group>`, with the group id that the journal holds. Source: `src/commands/undo.rs` (lines 17-34). See the [undo command](commands.md#relaxkit-undo).

## Files relaxkit writes

The tables list the files that relaxkit writes, and the files that the Claude Code and OpenCode commands write when relaxkit runs them. They are not a complete inventory: npm, uv and the host CLIs write files of their own, which these tables do not list. The paths are the defaults for macOS, where the runs were made in a disposable HOME. The relaxkit paths are the same on Linux, because `src/platform.rs` has no operating-system branch for them. The rtk and Playwright paths differ, and the table gives both.

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
| `~/.claude/settings.json`, key `hooks.PreToolUse` | A `Bash` hook entry whose command runs the pack's rtk copy with `hook claude`. rtk's install step writes it once, and the other hooks are kept. Source: `src/config_apply/claude.rs` (`apply_rtk_hook`, lines 166-185), `src/tools/handlers.rs` (`rtk_claude_hook`, lines 236-250). | Source |
| `~/.claude/settings.json`, key `env.DISABLE_TELEMETRY` | The value `"1"`. Written only by `relaxkit privacy --claude-code`, and it also stops feature-flag fetching. Source: `src/config_apply/claude.rs` (`disable_claude_code_telemetry`, lines 213-222), `src/commands/privacy.rs` (line 32). | Source |
| `~/.claude/CLAUDE.md` | The roles block, between the markers `<!-- relaxkit:begin roles -->` and `<!-- relaxkit:end roles -->`. | Observed |
| `~/.claude.json` | Claude Code's state file, which `claude mcp` and `claude plugin` write. Source: `src/host/claude.rs`. | Source |
| `~/.claude/plugins/known_marketplaces.json` and `installed_plugins.json` | Claude Code's plugin registry, which `claude plugin` writes. Source: `src/host/claude.rs`. | Source |

The Claude Code files are in `~/.claude` by default.

### OpenCode

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.config/opencode/opencode.json`, or `opencode.jsonc` when that file exists, key `agents` | The model of each relaxkit agent. Source: `src/config_apply/opencode.rs` (`apply_roles`). | Observed for `opencode.json` |
| The same file, key `plugins` | The entry `./plugins/caveman`, which is caveman's V2 plugin key. relaxkit does not write the V1 `plugin` key. Source: `src/config_apply/caveman.rs` (`PLUGIN_SPEC`, line 83; `add_plugin_entry`, line 223), `src/config_apply/opencode.rs` (line 3). | Source |
| The same file, key `mcp.servers.<name>` | The MCP entries. relaxkit adds one with `opencode mcp add --global`, and it removes the old entry before it adds the new one. It reads the entries to check them. Source: `src/host/opencode.rs` (`mcp_add`, line 210; the lookup at line 236; `mcp_remove`, line 252). | Source |
| `~/.config/opencode/AGENTS.md` | The routing block, and a `caveman` block with caveman's ruleset. Each block is between its own managed markers, so the rest of the file is kept. Source: `src/config_apply/opencode.rs` (`ROUTING_BLOCK`), `src/config_apply/caveman.rs` (`BLOCK_ID`, line 80). | Routing block observed; `caveman` block Source |
| `~/.config/opencode/plugins/caveman/` | Five files: `plugin.js`, `server.js`, `package.json`, `caveman-config.cjs` and `caveman-parse.cjs`. The last two are renamed from `.js` on copy. Written by the `caveman_opencode` step of setup and update. Source: `src/config_apply/caveman.rs` (`PLUGIN_FILES`, line 21). | Source |
| `~/.config/opencode/commands/` | Eight command files: `caveman.md`, `ultracave.md`, `megacave.md`, `caveman-commit.md`, `caveman-review.md`, `caveman-compress.md`, `caveman-stats.md` and `caveman-help.md`. Source: `src/config_apply/caveman.rs` (`COMMAND_FILES`, line 45). | Source |
| `~/.config/opencode/agents/` | Three subagent files: `cavecrew-investigator.md`, `cavecrew-builder.md` and `cavecrew-reviewer.md`. Source: `src/config_apply/caveman.rs` (`AGENT_FILES`, line 57). | Source |
| `~/.config/opencode/skills/` | Nine skill directories, copied whole: `caveman`, `ultracave`, `megacave`, `caveman-commit`, `caveman-review`, `caveman-help`, `caveman-stats`, `caveman-compress` and `cavecrew`. Source: `src/config_apply/caveman.rs` (`SKILL_DIRS`, line 64). | Source |

The OpenCode files are in `~/.config/opencode` by default.

### Caveman and rtk

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.config/caveman/config.json` | The voice, as `defaultMode`. | Observed |
| `~/Library/Application Support/rtk/config.toml` on macOS, `~/.config/rtk/config.toml` on Linux | rtk's config. relaxkit sets `[display] pro_hint = false`, which hides rtk's Pro notice. The Linux path is marked in `src/platform.rs` as still to verify. Source: `src/platform.rs`, `src/runtime/rtk.rs`. | Source |

### Serena

| Path | What it holds | Status |
| --- | --- | --- |
| `~/.serena/serena_config.yml` | The Serena config file. `serena init` writes it, and the serena step of setup and update runs `serena init`. Observed by running `serena init` in a disposable HOME with the serena binary on this machine. Source: `src/tools/handlers.rs` (`serena_init`, lines 102-110). | Observed |

### Playwright

| Path | What it holds | Status |
| --- | --- | --- |
| `~/Library/Caches/ms-playwright` on macOS, `~/.cache/ms-playwright` on Linux | Where Playwright keeps Chromium. relaxkit looks there for the browser. Source: `src/platform.rs`, `src/runtime/chromium.rs`. | Source |
