// @ts-check
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildConfig } from "../src/config.js";

const base = { directory: "/work/proj", worktree: "/work/proj", home: "/home/u", tmpdir: "/tmp" };

test("mode is enforce", () => {
  const cfg = buildConfig({ ...base, env: {} });
  assert.equal(cfg.mode, "enforce");
});

test("allowed domains are exactly the seven relaxkit hosts", () => {
  const cfg = buildConfig({ ...base, env: {} });
  assert.deepEqual(cfg.network.allowedDomains, [
    "github.com",
    "api.github.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
  ]);
});

test("denyWrite holds exactly the four required entries", () => {
  const cfg = buildConfig({ ...base, env: {} });
  assert.equal(cfg.filesystem.denyWrite.length, 4);
  assert.ok(cfg.filesystem.denyWrite.includes(".opencode"));
  assert.ok(cfg.filesystem.denyWrite.includes("opencode.json"));
  assert.ok(cfg.filesystem.denyWrite.includes("opencode.jsonc"));
  assert.ok(cfg.filesystem.denyWrite.includes("/home/u/.config/opencode"));
});

test("denyWrite uses XDG_CONFIG_HOME for the OpenCode config directory", () => {
  const cfg = buildConfig({ ...base, env: { XDG_CONFIG_HOME: "/xdg" } });
  assert.ok(cfg.filesystem.denyWrite.includes("/xdg/opencode"));
});

test("sandbox-switching env vars do not change the result", () => {
  const plain = buildConfig({ ...base, env: {} });
  const switched = buildConfig({
    ...base,
    env: {
      OPENCODE_SANDBOX_CONFIG: JSON.stringify({ mode: "warn", disabled: true }),
      OPENCODE_SANDBOX_CONFIG_PATH: "/etc/opencode-sandbox.json",
      OPENCODE_DISABLE_SANDBOX: "1",
    },
  });
  assert.deepEqual(switched, plain);
  assert.equal(switched.mode, "enforce");
});

test("allowWrite defaults to the project, worktree and tmpdir", () => {
  const cfg = buildConfig({ ...base, env: {} });
  assert.deepEqual(cfg.filesystem.allowWrite, ["/work/proj", "/tmp"]);
  assert.deepEqual(cfg.filesystem.allowRead, []);
  assert.ok(cfg.filesystem.denyRead.includes("/home/u/.ssh"));
});
