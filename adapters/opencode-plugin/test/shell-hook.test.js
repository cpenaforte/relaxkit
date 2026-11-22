// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import { createShellHook } from "../src/shell-hook.js";

/** @param {string} command */
function event(command) {
  return { command, shell: "/bin/zsh", cwd: "/work/proj", timeout: 0, env: /** @type {Record<string, string | undefined>} */ ({ PATH: "/usr/bin" }) };
}

/**
 * Runs the hook on a command and returns the env it left on the event.
 * @param {string} command
 */
async function envFor(command) {
  /** @type {string[]} */
  const wrapped = [];
  const hook = createShellHook({
    /** @param {{ command: string }} ev */
    async wrap(ev) {
      wrapped.push(ev.command);
    },
  });
  const ev = event(command);
  await hook(ev);
  assert.deepEqual(wrapped, [command]);
  return ev.env;
}

test("the env has SUPERPOWERS_DISABLE_TELEMETRY set to 1", async () => {
  const env = await envFor("echo hi");
  assert.equal(env.SUPERPOWERS_DISABLE_TELEMETRY, "1");
});

for (const command of ["gh pr view 1", "echo hi", "npm test"]) {
  test(`no GH_TOKEN key is set for ${JSON.stringify(command)}`, async () => {
    const env = await envFor(command);
    assert.equal(Object.prototype.hasOwnProperty.call(env, "GH_TOKEN"), false);
    assert.equal(env.PATH, "/usr/bin");
  });
}
