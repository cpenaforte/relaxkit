// @ts-check
import { spawn as nodeSpawn } from "node:child_process";
import { formatFailure, runProcess } from "./process.js";

/**
 * @typedef {import("@opencode/plugin").Plugin.Context["permission"]} PermissionDomain
 * @typedef {typeof nodeSpawn} SpawnFn
 */

/**
 * The first argument must be one of these. Taken from `gh --help` (gh version 2.105.0):
 * the CORE COMMANDS, GITHUB ACTIONS COMMANDS and ADDITIONAL COMMANDS sections, plus
 * `help` and `version`. Then `alias`, `config`, `extension` and `auth` are removed,
 * because they are always refused. The list is a snapshot: when gh adds or renames a
 * built-in command, update this list. `gh <name>` for a name that is not built in
 * runs an installed alias or extension outside the sandbox, so unknown names are refused.
 */
export const GH_BUILTIN_COMMANDS = [
  // CORE COMMANDS (auth removed)
  "browse",
  "codespace",
  "discussion",
  "gist",
  "issue",
  "org",
  "pr",
  "project",
  "release",
  "repo",
  "skill",
  // GITHUB ACTIONS COMMANDS
  "cache",
  "run",
  "workflow",
  // ADDITIONAL COMMANDS (alias, config, extension removed)
  "agent-task",
  "api",
  "attestation",
  "completion",
  "copilot",
  "event",
  "gpg-key",
  "label",
  "licenses",
  "preview",
  "ruleset",
  "search",
  "secret",
  "ssh-key",
  "status",
  "variable",
  // help and version
  "help",
  "version",
];

export const GH_DESCRIPTION =
  "Run the GitHub CLI (gh) with the given arguments. Use this tool instead of gh in the shell: shell gh runs inside the sandbox and cannot reach GitHub.";

/**
 * Builds the gh tool. It runs gh outside the sandbox, with the project directory as cwd
 * and no change to the environment.
 * @param {{ spawn?: SpawnFn, permission: Pick<PermissionDomain, "assert">, cwd: string }} options
 */
export function createGhTool({ spawn = nodeSpawn, permission, cwd }) {
  return {
    name: "gh",
    description: GH_DESCRIPTION,
    input: {
      type: "object",
      properties: { args: { type: "array", items: { type: "string" } } },
      required: ["args"],
      additionalProperties: false,
    },
    /**
     * @param {unknown} input
     * @param {{ sessionID: string, messageID: string, id: string }} context
     * @returns {Promise<{ content: string }>}
     */
    execute: async (input, context) => {
      const args = /** @type {{ args?: unknown } | undefined} */ (input)?.args;
      if (!Array.isArray(args) || args.length === 0 || !args.every((a) => typeof a === "string")) {
        return { content: "Refused: gh needs at least one argument." };
      }
      const subcommand = /** @type {string} */ (args[0]);
      if (!GH_BUILTIN_COMMANDS.includes(subcommand)) {
        return {
          content: `Refused: "gh ${subcommand}" is not a built-in gh command, so this tool does not run it.`,
        };
      }
      // A denial rethrows here, so it ends the step. It is not returned as tool output.
      await permission.assert({
        sessionID: context.sessionID,
        action: "relaxkit.gh",
        resources: [subcommand],
        source: { type: "tool", messageID: context.messageID, id: context.id },
      });
      const result = await runProcess(spawn, "gh", args, { shell: false, cwd });
      return { content: result.code === 0 ? result.stdout : formatFailure(result) };
    },
  };
}
