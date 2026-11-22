// @ts-check
import { spawn as nodeSpawn } from "node:child_process";
import { formatFailure, runProcess } from "./process.js";

/**
 * @typedef {import("@opencode/plugin").Plugin.Context["permission"]} PermissionDomain
 * @typedef {typeof nodeSpawn} SpawnFn
 */

/** Subcommands that are refused: alias and extension run code, config and auth touch credentials. */
export const GH_REFUSED = ["alias", "extension", "config", "auth"];

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
      if (GH_REFUSED.includes(subcommand)) {
        return { content: `Refused: "gh ${subcommand}" is not available through this tool.` };
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
