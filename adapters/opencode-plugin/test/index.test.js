// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import plugin from "../src/index.js";

/** Fake SandboxManager: initialize and wrap succeed, cleanup is a no-op. */
const fakeManager = {
  /** @returns {Promise<void>} */
  async initialize() {},
  /** @param {string} command */
  async wrapWithSandbox(command) {
    return `sandboxed:${command}`;
  },
  cleanupAfterCommand() {},
};

function fakeCtx() {
  /** @type {any[]} */
  const tools = [];
  /** @type {any[]} */
  const toolUpdates = [];
  /** @type {any[]} */
  const shellHooks = [];
  /** @type {any[]} */
  const commands = [];
  const ctx = {
    location: { directory: "/work/proj", project: { directory: "/work/proj" } },
    tool: {
      /** @param {(editor: any) => void} cb */
      async transform(cb) {
        cb({
          /** @param {any} t */
          add: (t) => tools.push(t),
          /** @param {string} id @param {(t: any) => void} update */
          update: (id, update) => {
            toolUpdates.push({ id, update });
          },
        });
        return { dispose: async () => {} };
      },
    },
    shell: {
      /** @param {string} name @param {any} fn */
      async hook(name, fn) {
        shellHooks.push({ name, fn });
        return { dispose: async () => {} };
      },
    },
    command: {
      /** @param {(editor: any) => void} cb */
      async transform(cb) {
        cb({ add: (/** @type {any} */ d) => commands.push(d) });
        return { dispose: async () => {} };
      },
    },
    permission: {
      async assert() {},
    },
    session: {
      async synthetic() {
        return {};
      },
    },
  };
  return { ctx, tools, toolUpdates, shellHooks, commands };
}

/** @returns {any} */
function deps() {
  return {
    SandboxManager: fakeManager,
    spawn: () => {
      throw new Error("spawn must not run in this test");
    },
    prepareTmp: async () => {},
  };
}

test("the default export is the plain object { id, setup }", () => {
  assert.equal(plugin.id, "relaxkit");
  assert.equal(typeof plugin.setup, "function");
  assert.deepEqual(Object.keys(plugin).sort(), ["id", "setup"]);
  assert.equal(Object.getPrototypeOf(plugin), Object.prototype);
});

test("setup registers the gh tool, the shell hook and the eleven commands", async () => {
  const { ctx, tools, toolUpdates, shellHooks, commands } = fakeCtx();
  const cleanup = await /** @type {any} */ (plugin).setup(/** @type {any} */ (ctx), deps());
  try {
    assert.deepEqual(tools.map((t) => t.name), ["gh"]);
    assert.ok(toolUpdates.some((u) => u.id === "shell"));
    assert.deepEqual(shellHooks.map((h) => h.name), ["create.before"]);
    assert.equal(commands.length, 11);
  } finally {
    if (typeof cleanup === "function") await cleanup();
  }
});
