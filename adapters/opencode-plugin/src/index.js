// @ts-check
import { spawn as nodeSpawn } from "node:child_process";
import { buildConfig } from "./config.js";
import { registerCommands } from "./commands.js";
import { createGhTool } from "./gh-tool.js";
import { createSandbox } from "./sandbox.js";
import { createShellHook } from "./shell-hook.js";

/**
 * @typedef {import("@opencode/plugin").Plugin.Plugin} Plugin
 * @typedef {import("@opencode/plugin").Plugin.Context} Context
 * @typedef {{ SandboxManager?: any, spawn?: typeof nodeSpawn, prepareTmp?: () => Promise<void> }} SetupDeps
 */

/**
 * Sets up the relaxkit adapter: the shell hook, the gh tool, and the eleven commands.
 * `deps` is for tests. Production calls pass only ctx.
 * @param {Context} ctx
 * @param {SetupDeps} [deps]
 */
async function setup(ctx, deps = {}) {
  const directory = ctx.location.directory;
  const worktree = ctx.location.project.directory;

  const config = buildConfig({ directory, worktree, env: process.env });
  const sandbox = await createSandbox({
    config,
    directory,
    worktree,
    manager: deps.SandboxManager,
    prepareTmp: deps.prepareTmp,
  });

  await ctx.shell.hook("create.before", createShellHook(sandbox));

  await ctx.tool.transform((editor) => {
    editor.update("shell", (tool) => {
      const execute = tool.execute;
      tool.execute = async (input, context) => {
        const command = /** @type {any} */ (input)?.command;
        if (typeof command !== "string" || !command) return execute(input, context);
        return sandbox.runShellCall({ id: context.id, command, signal: context.signal }, () =>
          execute(input, context),
        );
      };
    });
    editor.add(createGhTool({ spawn: deps.spawn, permission: ctx.permission, cwd: worktree }));
  });

  await registerCommands(ctx, deps.spawn);

  return async () => {
    await sandbox.dispose();
  };
}

/** @type {Plugin} */
const plugin = { id: "relaxkit", setup };

export default plugin;
