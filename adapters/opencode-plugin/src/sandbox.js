// @ts-check
// Fork of the hook logic of opencode-sandbox 0.7.0 (MIT, Copyright (c) 2026 Iván Sánchez).
// Forked 2026-11-22. Upstream src/v2.ts ported: initialize (361-390), the /tmp/claude
// handling (374-385), the denyWrite and path resolution (402-413), cleanup (419-430),
// the shell create.before hook (455-477 and 486-488), and the shell tool wrapper
// (501-571, without shell.exited tracking). The config loader and the
// OPENCODE_DISABLE_SANDBOX check are not ported. See NOTICE.md.
import { AsyncLocalStorage } from "node:async_hooks";
import { lstat, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { SandboxManager as NodeSandboxManager } from "@anthropic-ai/sandbox-runtime";

/**
 * @typedef {import("@anthropic-ai/sandbox-runtime").SandboxRuntimeConfig} SandboxRuntimeConfig
 * @typedef {{ command: string, shell: string, env: Record<string, string | undefined> }} ShellEvent
 * @typedef {{ id?: string, command: string, signal?: AbortSignal }} ShellCall
 * @typedef {{ command: string, wrapped: boolean, cleaned: boolean, signal?: AbortSignal, id?: string }} CallState
 */

/** The error text the user sees when the sandbox cannot run a command. */
export const ENFORCEMENT_ERROR = "Sandbox unavailable in enforce mode; command blocked";

/** Upstream key, shared with opencode-sandbox so both see one SandboxManager state. */
const RUNTIME_KEY = Symbol.for("opencode-sandbox-v2.runtime-state");
const NETWORK_CONFLICT = "Conflicting sandbox network policies in this OpenCode process";

/** Upstream launcher: the shell runs the wrapped command from OPENCODE_SANDBOX_WRAPPED. */
const LAUNCHER = `#!/bin/sh
sandbox_command=$OPENCODE_SANDBOX_WRAPPED
sandbox_shell=$OPENCODE_SANDBOX_SHELL
unset OPENCODE_SANDBOX_WRAPPED OPENCODE_SANDBOX_SHELL
if [ "$1" = "+o" ]; then
  exec "$sandbox_shell" "$1" "$2" "$3" "$sandbox_command"
fi
exec "$sandbox_shell" "$1" "$sandbox_command"
`;

/**
 * Upstream /tmp/claude handling (v2.ts 374-385). Skipped when CLAUDE_CODE_TMPDIR or
 * CLAUDE_TMPDIR is set. Otherwise it creates /tmp/claude with mode 0700 (or reuses it)
 * and fails unless it is a directory owned by the current user with no group or
 * other write bits. The sandbox runtime uses it as its temporary directory.
 * @param {Record<string, string | undefined>} [env]
 * @returns {Promise<void>}
 */
export async function ensureClaudeTmpDir(env = process.env) {
  if (env.CLAUDE_CODE_TMPDIR || env.CLAUDE_TMPDIR) return;
  try {
    await mkdir("/tmp/claude", { mode: 0o700 });
  } catch (error) {
    if (/** @type {NodeJS.ErrnoException} */ (error).code !== "EEXIST") throw error;
  }
  const temp = await lstat("/tmp/claude");
  if (!temp.isDirectory() || temp.uid !== process.getuid?.() || temp.mode & 0o22) {
    throw new Error("Unsafe sandbox temporary directory: /tmp/claude");
  }
}

/**
 * Upstream initialize (v2.ts 361-390), with the manager passed in.
 * @param {any} manager
 * @param {SandboxRuntimeConfig} config
 * @param {() => Promise<void>} prepareTmp
 */
async function initialize(manager, config, prepareTmp) {
  const runtime = /** @type {any} */ (globalThis);
  runtime[RUNTIME_KEY] ??= new WeakMap();
  /** @type {{ network?: string, initialization?: Promise<void> } | undefined} */
  let state = runtime[RUNTIME_KEY].get(manager);
  if (!state) {
    state = {};
    runtime[RUNTIME_KEY].set(manager, state);
  }
  const network = JSON.stringify(config.network);
  if (state.network && state.network !== network) throw new Error(NETWORK_CONFLICT);
  if (!state.initialization) {
    state.network = network;
    state.initialization = (async () => {
      await prepareTmp();
      await manager.initialize(config);
    })();
  }
  await state.initialization;
}

/**
 * @typedef {object} SandboxOptions
 * @property {import("./config.js").SandboxConfig} config
 * @property {string} directory The project location; relative config entries resolve against it.
 * @property {any} [manager] SandboxManager. Injected in tests.
 * @property {() => Promise<void>} [prepareTmp] Injected in tests. Defaults to ensureClaudeTmpDir.
 * @property {string} [platform] Defaults to process.platform.
 */

/**
 * Creates the sandbox for one OpenCode session. `wrap` is the shell hook body. It
 * rewrites the shell event so the command runs through the sandbox, and it throws
 * ENFORCEMENT_ERROR when that cannot be done. `runShellCall` wraps one shell tool
 * call so that cleanupAfterCommand runs when the call ends.
 * @param {SandboxOptions} options
 */
export async function createSandbox({
  config,
  directory,
  manager = NodeSandboxManager,
  prepareTmp = () => ensureClaudeTmpDir(),
  platform = process.platform,
}) {
  // Upstream setup (402-405): relative entries resolve against the project directory.
  /** @param {string} p */
  const resolvePath = (p) => (p.startsWith("~") || path.isAbsolute(p) ? p : path.resolve(directory, p));
  /** @type {SandboxRuntimeConfig} */
  const runtimeConfig = /** @type {any} */ ({
    filesystem: {
      denyRead: config.filesystem.denyRead.map(resolvePath),
      allowRead: config.filesystem.allowRead.map(resolvePath),
      allowWrite: config.filesystem.allowWrite.map(resolvePath),
      denyWrite: config.filesystem.denyWrite.map(resolvePath),
    },
    network: config.network,
  });

  // Upstream setup (406-413): a private temp directory is added to denyWrite for the wrapped command.
  const tmpDir = await mkdtemp(path.join(os.tmpdir(), "relaxkit-sandbox-"));
  /** @type {SandboxRuntimeConfig} */
  const guarded = /** @type {any} */ ({
    ...runtimeConfig,
    filesystem: { ...runtimeConfig.filesystem, denyWrite: [...(runtimeConfig.filesystem?.denyWrite ?? []), tmpDir] },
  });

  const calls = new AsyncLocalStorage();

  /** @param {string} message */
  const log = (message) => console.warn(`[relaxkit-opencode] ${message}`);

  /** @param {CallState | undefined} call */
  const cleanup = (call) => {
    if (!call || call.cleaned || !call.wrapped) return;
    call.cleaned = true;
    try {
      manager.cleanupAfterCommand();
    } catch (error) {
      log(`Failed to clean up sandbox mount points: ${String(error)}`);
    }
  };

  /**
   * The shell create.before hook body (upstream 455-477 and 486-488). It always wraps
   * the command it is given, with or without a tool call in scope.
   * @param {ShellEvent} event
   */
  const wrap = async (event) => {
    /** @type {CallState | undefined} */
    const call = calls.getStore();
    try {
      if (platform === "win32") throw new Error("Windows command-string sandboxing is unsupported");
      await initialize(manager, runtimeConfig, prepareTmp);
      const wrapped = await manager.wrapWithSandbox(event.command, event.shell, guarded, call?.signal, {
        commandId: call?.id,
        commandText: event.command,
      });
      if (call) call.wrapped = true;
      if (call?.signal?.aborted) {
        cleanup(call);
        throw new Error("Sandboxed command was interrupted");
      }
      const executable = path.join(tmpDir, path.basename(event.shell));
      try {
        await writeFile(executable, LAUNCHER, { mode: 0o700, flag: "wx" });
      } catch (error) {
        if (/** @type {NodeJS.ErrnoException} */ (error).code !== "EEXIST") throw error;
      }
      event.env.OPENCODE_SANDBOX_WRAPPED = wrapped;
      event.env.OPENCODE_SANDBOX_SHELL = event.shell;
      event.shell = executable;
    } catch (error) {
      cleanup(call);
      const message = error instanceof Error ? error.message : String(error);
      log(`${message}; command blocked`);
      throw new Error(ENFORCEMENT_ERROR, { cause: error });
    }
  };

  /**
   * Runs one shell tool call in its own async context, so the hook can see it, and
   * cleans up the sandbox when the call ends.
   * @template T
   * @param {ShellCall} call
   * @param {() => Promise<T>} run
   * @returns {Promise<T>}
   */
  const runShellCall = async (call, run) => {
    /** @type {CallState} */
    const state = { id: call.id, command: call.command, signal: call.signal, wrapped: false, cleaned: false };
    try {
      return await calls.run(state, run);
    } finally {
      cleanup(state);
    }
  };

  /** Upstream cleanup (574-575): removes the private temp directory. */
  const dispose = async () => {
    await rm(tmpDir, { recursive: true, force: true });
  };

  return { wrap, runShellCall, dispose };
}
