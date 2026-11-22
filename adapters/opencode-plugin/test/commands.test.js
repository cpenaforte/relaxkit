// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { registerCommands, COMMAND_NAMES, MUTATING_COMMANDS } from "../src/commands.js";

/**
 * A fake spawn that records every call and emits the given output on the next tick.
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

/** Builds a fake ctx and collects what the commands do. */
function fakeCtx() {
  /** @type {any[]} */
  const commands = [];
  /** @type {any[]} */
  const asserts = [];
  /** @type {any[]} */
  const synthetic = [];
  /** @type {string[]} */
  const order = [];
  const ctx = {
    location: { directory: "/work/proj", project: { directory: "/work/proj" } },
    command: {
      /** @param {(editor: any) => void} cb */
      async transform(cb) {
        cb({ add: (/** @type {any} */ definition) => commands.push(definition) });
        return { dispose: async () => {} };
      },
    },
    permission: {
      /** @param {any} input */
      async assert(input) {
        asserts.push(input);
        order.push(`assert:${input.action}`);
      },
    },
    session: {
      /** @param {any} input */
      async synthetic(input) {
        synthetic.push(input);
        return {};
      },
    },
  };
  return { ctx, commands, asserts, synthetic, order };
}

/** @param {any} commands @param {string} name */
function commandNamed(commands, name) {
  const found = commands.find((/** @type {any} */ c) => c.name === name);
  assert.ok(found, `command ${name} is registered`);
  return found;
}

const invocation = { sessionID: "ses_1", prompt: { text: "" }, delivery: "queue" };

test("exactly eleven commands are registered, one per relaxkit subcommand", async () => {
  const { ctx, commands } = fakeCtx();
  await registerCommands(/** @type {any} */ (ctx), fakeSpawn().spawn);
  assert.equal(commands.length, 11);
  assert.deepEqual(commands.map((c) => c.name).sort(), [...COMMAND_NAMES].sort());
  assert.deepEqual(
    [...COMMAND_NAMES].sort(),
    ["config", "doctor", "login", "preset", "privacy", "setup", "status", "undo", "uninstall", "update", "usage"],
  );
});

for (const name of MUTATING_COMMANDS) {
  test(`mutating command ${name} asserts before spawning`, async () => {
    const { ctx, commands, asserts, order } = fakeCtx();
    const fake = fakeSpawn({ stdout: "ok\n" });
    /** @type {any} */
    const spawn = (/** @type {string} */ c, /** @type {string[]} */ a, /** @type {any} */ o) => {
      order.push("spawn");
      return fake.spawn(c, a, o);
    };
    await registerCommands(/** @type {any} */ (ctx), spawn);
    await commandNamed(commands, name).execute({ ...invocation, sessionID: "ses_1" });
    assert.deepEqual(order, [`assert:relaxkit.${name}`, "spawn"]);
    assert.equal(asserts.length, 1);
    assert.equal(asserts[0].action, `relaxkit.${name}`);
    assert.deepEqual(asserts[0].resources, [name]);
    assert.equal(asserts[0].sessionID, "ses_1");
    assert.equal(fake.calls.length, 1);
    assert.deepEqual(fake.calls[0].args, [name]);
  });
}

for (const name of ["status", "usage"]) {
  test(`${name} does not ask for permission`, async () => {
    const { ctx, commands, asserts } = fakeCtx();
    const fake = fakeSpawn({ stdout: "fine\n" });
    await registerCommands(/** @type {any} */ (ctx), fake.spawn);
    await commandNamed(commands, name).execute(invocation);
    assert.equal(asserts.length, 0);
    assert.equal(fake.calls.length, 1);
  });
}

test("the spawn receives the subcommand, the user args, shell false", async () => {
  const { ctx, commands } = fakeCtx();
  const fake = fakeSpawn({ stdout: "" });
  await registerCommands(/** @type {any} */ (ctx), fake.spawn);
  await commandNamed(commands, "preset").execute({ ...invocation, prompt: { text: "strict  extra" } });
  assert.equal(fake.calls[0].command, "relaxkit");
  assert.deepEqual(fake.calls[0].args, ["preset", "strict", "extra"]);
  assert.equal(fake.calls[0].options.shell, false);
});

test("a successful run delivers its stdout as the command output", async () => {
  const { ctx, commands, synthetic } = fakeCtx();
  await registerCommands(/** @type {any} */ (ctx), fakeSpawn({ stdout: "relaxkit 0.1.0\n" }).spawn);
  await commandNamed(commands, "status").execute(invocation);
  assert.equal(synthetic.length, 1);
  assert.equal(synthetic[0].text, "relaxkit 0.1.0\n");
  assert.equal(synthetic[0].sessionID, "ses_1");
});

test("a failing run delivers stderr and the exit code", async () => {
  const { ctx, commands, synthetic } = fakeCtx();
  await registerCommands(/** @type {any} */ (ctx), fakeSpawn({ stderr: "no such preset\n", code: 3 }).spawn);
  await commandNamed(commands, "preset").execute({ ...invocation, prompt: { text: "missing" } });
  assert.equal(synthetic.length, 1);
  assert.ok(synthetic[0].text.includes("no such preset"));
  assert.ok(synthetic[0].text.includes("3"));
});
