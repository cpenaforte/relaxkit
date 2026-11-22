// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import { createSandbox, ENFORCEMENT_ERROR } from "../src/sandbox.js";
import { buildConfig } from "../src/config.js";

/**
 * @param {{ initializeError?: Error }} [opts]
 * @returns {any}
 */
function fakeManager(opts = {}) {
  const initializeError = opts.initializeError;
  const manager = {
    initializeCalls: 0,
    initConfig: /** @type {any} */ (null),
    cleanupCalls: 0,
    wrapCalls: /** @type {string[]} */ ([]),
    order: /** @type {string[]} */ ([]),
    /** @param {any} config */
    async initialize(config) {
      manager.initializeCalls++;
      manager.initConfig = config;
      if (initializeError) throw initializeError;
    },
    /** @param {string} command */
    async wrapWithSandbox(command) {
      manager.wrapCalls.push(command);
      return `sandboxed:${command}`;
    },
    cleanupAfterCommand() {
      manager.cleanupCalls++;
      manager.order.push("cleanup");
    },
  };
  return manager;
}

const config = buildConfig({
  directory: "/work/proj",
  worktree: "/work/proj",
  env: {},
  home: "/home/u",
  tmpdir: "/tmp",
});

/** @param {string} command */
function shellEvent(command) {
  return { command, shell: "/bin/zsh", cwd: "/work/proj", timeout: 0, env: /** @type {Record<string, string | undefined>} */ ({}) };
}

test("the hook wraps the command with the sandbox", async () => {
  const manager = fakeManager();
  const sandbox = await createSandbox({ config, directory: "/work/proj", manager, prepareTmp: async () => {} });
  try {
    const event = shellEvent("echo hi");
    await sandbox.wrap(event);
    assert.deepEqual(manager.wrapCalls, ["echo hi"]);
    assert.equal(event.env.OPENCODE_SANDBOX_WRAPPED, "sandboxed:echo hi");
    assert.equal(event.env.OPENCODE_SANDBOX_SHELL, "/bin/zsh");
    assert.notEqual(event.shell, "/bin/zsh");
  } finally {
    await sandbox.dispose();
  }
});

test("when initialize fails the command is blocked with the enforce error", async () => {
  const manager = fakeManager({ initializeError: new Error("no bwrap") });
  const sandbox = await createSandbox({ config, directory: "/work/proj", manager, prepareTmp: async () => {} });
  try {
    await assert.rejects(() => sandbox.wrap(shellEvent("echo hi")), { message: ENFORCEMENT_ERROR });
    assert.deepEqual(manager.wrapCalls, []);
  } finally {
    await sandbox.dispose();
  }
});

test("relative denyWrite entries resolve against the location directory and the project root", async () => {
  const manager = fakeManager();
  const sandbox = await createSandbox({
    config,
    directory: "/root/sub",
    worktree: "/root",
    manager,
    prepareTmp: async () => {},
  });
  try {
    await sandbox.wrap(shellEvent("echo hi"));
    const denyWrite = manager.initConfig.filesystem.denyWrite;
    assert.ok(denyWrite.includes("/root/.opencode"));
    assert.ok(denyWrite.includes("/root/opencode.json"));
    assert.ok(denyWrite.includes("/root/opencode.jsonc"));
    assert.ok(denyWrite.includes("/root/sub/.opencode"));
    assert.ok(denyWrite.includes("/home/u/.config/opencode"));
  } finally {
    await sandbox.dispose();
  }
});

test("cleanupAfterCommand is called after the command", async () => {
  const manager = fakeManager();
  const sandbox = await createSandbox({ config, directory: "/work/proj", manager, prepareTmp: async () => {} });
  try {
    await sandbox.runShellCall({ command: "echo hi" }, async () => {
      await sandbox.wrap(shellEvent("echo hi"));
      manager.order.push("command-finished");
    });
    assert.equal(manager.cleanupCalls, 1);
    assert.deepEqual(manager.order, ["command-finished", "cleanup"]);
  } finally {
    await sandbox.dispose();
  }
});
