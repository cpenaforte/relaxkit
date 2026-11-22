// @ts-check
import { spawn as nodeSpawn } from "node:child_process";
import { formatFailure, runProcess } from "./process.js";

/**
 * @typedef {import("@opencode/plugin").Plugin.Context} Context
 * @typedef {typeof nodeSpawn} SpawnFn
 */

/** One command per relaxkit subcommand. */
export const COMMAND_NAMES = [
  "setup",
  "status",
  "doctor",
  "update",
  "config",
  "preset",
  "privacy",
  "login",
  "undo",
  "uninstall",
  "usage",
];

/**
 * The subcommands that change the machine (the PACK_COMMANDS set in src/sandbox/claude.rs).
 * Each one asks for permission before it runs.
 */
export const MUTATING_COMMANDS = ["setup", "update", "config", "preset", "privacy", "login", "undo", "uninstall", "doctor"];

/**
 * Splits the text the user typed after the command into arguments. No shell is involved.
 * @param {string} text
 * @returns {string[]}
 */
export function splitArgs(text) {
  return text.trim().split(/\s+/).filter(Boolean);
}

/**
 * Runs `relaxkit <subcommand> ...args` and returns the text to show. Stdout on
 * success; stderr and the exit code otherwise.
 * @param {string} subcommand
 * @param {string[]} args
 * @param {{ spawn: SpawnFn, cwd: string }} deps
 * @returns {Promise<string>}
 */
export async function runRelaxkit(subcommand, args, { spawn, cwd }) {
  const result = await runProcess(spawn, "relaxkit", [subcommand, ...args], { shell: false, cwd });
  return result.code === 0 ? result.stdout : formatFailure(result);
}

/**
 * Registers the eleven relaxkit commands. Mutating ones call permission.assert
 * before spawning. The output is delivered to the session as a synthetic message,
 * because the command API returns nothing.
 * @param {Context} ctx
 * @param {SpawnFn} [spawn]
 */
export function registerCommands(ctx, spawn = nodeSpawn) {
  const cwd = ctx.location.project.directory;
  const mutating = new Set(MUTATING_COMMANDS);
  return ctx.command.transform((editor) => {
    for (const name of COMMAND_NAMES) {
      editor.add({
        name,
        description: `Run relaxkit ${name}`,
        execute: async (input) => {
          if (mutating.has(name)) {
            await ctx.permission.assert({
              sessionID: input.sessionID,
              action: `relaxkit.${name}`,
              resources: [name],
            });
          }
          const text = await runRelaxkit(name, splitArgs(input.prompt.text), { spawn, cwd });
          await ctx.session.synthetic({ sessionID: input.sessionID, text });
        },
      });
    }
  });
}
