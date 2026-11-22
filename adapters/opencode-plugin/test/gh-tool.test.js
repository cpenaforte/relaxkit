// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createGhTool } from "../src/gh-tool.js";

/**
 * @param {{ stdout?: string, stderr?: string, code?: number }} result
 */
function fakeSpawn(result = {}) {
  /** @type {any[]} */
  const calls = [];
  /** @type {any} */
  const spawn = (/** @type {string} */ command, /** @type {string[]} */ args, /** @type {any} */ options) => {
    calls.push({ command, args, options });
    const child = /** @type {any} */ (new EventEmitter());
    child.stdout = new EventEmitter();
    child.stderr = new EventEmitter();
    setImmediate(() => {
      if (result.stdout) child.stdout.emit("data", Buffer.from(result.stdout));
      if (result.stderr) child.stderr.emit("data", Buffer.from(result.stderr));
      child.emit("close", result.code ?? 0);
    });
    return child;
  };
  return { spawn, calls };
}

/** @param {{ order?: string[] }} [opts] */
function fakePermission(opts = {}) {
  /** @type {any[]} */
  const asserts = [];
  const permission = {
    /** @param {any} input */
    async assert(input) {
      asserts.push(input);
      opts.order?.push("assert");
    },
  };
  return { permission, asserts };
}

const toolContext = {
  sessionID: "ses_1",
  messageID: "msg_1",
  agent: "build",
  id: "call_1",
  signal: new AbortController().signal,
  progress: async () => {},
};

/** @param {any} tool @param {string[]} args */
function run(tool, args) {
  return tool.execute({ args }, toolContext);
}

test("the tool is named gh and takes an args array", () => {
  const { spawn } = fakeSpawn();
  const { permission } = fakePermission();
  const tool = createGhTool({ spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  assert.equal(tool.name, "gh");
  assert.deepEqual(tool.input, {
    type: "object",
    properties: { args: { type: "array", items: { type: "string" } } },
    required: ["args"],
    additionalProperties: false,
  });
  assert.ok(tool.description.includes("shell gh runs inside the sandbox"));
});

for (const refused of ["alias", "extension", "config", "auth"]) {
  test(`refuses args[0] = ${refused} without spawning`, async () => {
    const fake = fakeSpawn();
    const { permission, asserts } = fakePermission();
    const tool = createGhTool({ spawn: fake.spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
    const result = await run(tool, [refused, "list"]);
    assert.ok(result.content.includes(refused));
    assert.equal(fake.calls.length, 0);
    assert.equal(asserts.length, 0);
  });
}

test("refuses empty args without spawning", async () => {
  const fake = fakeSpawn();
  const { permission } = fakePermission();
  const tool = createGhTool({ spawn: fake.spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  const result = await run(tool, []);
  assert.equal(typeof result.content, "string");
  assert.equal(fake.calls.length, 0);
});

test("spawns gh with shell false, the args, and the project cwd", async () => {
  const fake = fakeSpawn({ stdout: "#1 open\n" });
  const { permission } = fakePermission();
  const tool = createGhTool({ spawn: fake.spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  const result = await run(tool, ["pr", "view", "1"]);
  assert.equal(fake.calls.length, 1);
  assert.equal(fake.calls[0].command, "gh");
  assert.deepEqual(fake.calls[0].args, ["pr", "view", "1"]);
  assert.equal(fake.calls[0].options.shell, false);
  assert.equal(fake.calls[0].options.cwd, "/work/proj");
  assert.equal(result.content, "#1 open\n");
});

test("permission is asserted before spawn with action relaxkit.gh", async () => {
  /** @type {string[]} */
  const order = [];
  const fake = fakeSpawn();
  const originalSpawn = fake.spawn;
  /** @type {any} */
  const spawn = (/** @type {string} */ c, /** @type {string[]} */ a, /** @type {any} */ o) => {
    order.push("spawn");
    return originalSpawn(c, a, o);
  };
  const { permission, asserts } = fakePermission({ order });
  const tool = createGhTool({ spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  await run(tool, ["pr", "view", "1"]);
  assert.deepEqual(order, ["assert", "spawn"]);
  assert.equal(asserts[0].action, "relaxkit.gh");
  assert.deepEqual(asserts[0].resources, ["pr"]);
  assert.equal(asserts[0].sessionID, "ses_1");
});

test("the spawn env carries no GH_TOKEN", async () => {
  const fake = fakeSpawn();
  const { permission } = fakePermission();
  const tool = createGhTool({ spawn: fake.spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  await run(tool, ["pr", "view", "1"]);
  const env = fake.calls[0].options.env;
  assert.ok(!env || !("GH_TOKEN" in env));
});

test("a non-zero exit returns stderr and the exit code as text", async () => {
  const fake = fakeSpawn({ stderr: "could not resolve to a PullRequest\n", code: 1 });
  const { permission } = fakePermission();
  const tool = createGhTool({ spawn: fake.spawn, permission: /** @type {any} */ (permission), cwd: "/work/proj" });
  const result = await run(tool, ["pr", "view", "999"]);
  assert.ok(result.content.includes("could not resolve to a PullRequest"));
  assert.ok(result.content.includes("1"));
});
