// @ts-check
import os from "node:os";
import path from "node:path";

/**
 * The seven hosts the sandbox lets through. Same list as ALLOWED_DOMAINS in src/sandbox/mod.rs.
 * @type {readonly string[]}
 */
export const ALLOWED_DOMAINS = [
  "github.com",
  "api.github.com",
  "registry.npmjs.org",
  "pypi.org",
  "files.pythonhosted.org",
  "crates.io",
  "static.crates.io",
];

/** Upstream DEFAULT_DENY_READ_DIRS, relative to the home directory. */
const DENY_READ_DIRS = [
  ".ssh",
  ".gnupg",
  ".aws/credentials",
  ".azure",
  ".config/gcloud",
  ".config/gh",
  ".kube",
  ".docker/config.json",
  ".npmrc",
  ".netrc",
  ".env",
];

/** Upstream UNSAFE_WRITE_PATHS: a write grant on any of these is dropped. */
const UNSAFE_WRITE_PATHS = new Set([
  "/",
  "/home",
  "/usr",
  "/etc",
  "/var",
  "/opt",
  "/Library",
  "/System",
  "/private",
  "/Volumes",
  "/Users",
]);

/**
 * @typedef {object} SandboxConfig
 * @property {"enforce"} mode
 * @property {{ denyRead: string[], allowRead: string[], allowWrite: string[], denyWrite: string[] }} filesystem
 * @property {{ allowedDomains: string[], deniedDomains: string[], allowLocalBinding: boolean }} network
 */

/**
 * Builds the fixed sandbox configuration. Nothing is read from files, and no
 * OPENCODE_SANDBOX_* or OPENCODE_DISABLE_SANDBOX value changes the result. The
 * only env value read is XDG_CONFIG_HOME, which locates the OpenCode config
 * directory for denyWrite, as upstream getOpenCodeConfigDir does.
 * @param {{ directory: string, worktree: string, env?: Record<string, string | undefined>, home?: string, tmpdir?: string }} options
 * @returns {SandboxConfig}
 */
export function buildConfig({ directory, worktree, env = {}, home = os.homedir(), tmpdir = os.tmpdir() }) {
  const xdgConfigHome = env.XDG_CONFIG_HOME || path.join(home, ".config");
  const openCodeConfigDir = path.join(xdgConfigHome, "opencode");

  // Upstream allowWrite default: ${directory}, ${worktree}, ${tmpdir}, unsafe paths dropped, duplicates removed.
  /** @type {string[]} */
  const allowWrite = [];
  const seen = new Set();
  for (const candidate of [directory, worktree, tmpdir]) {
    const resolved = path.resolve(candidate);
    if (UNSAFE_WRITE_PATHS.has(resolved)) {
      console.warn(`[relaxkit-opencode] Rejecting unsafe write path: ${resolved}`);
      continue;
    }
    if (seen.has(resolved)) continue;
    seen.add(resolved);
    allowWrite.push(resolved);
  }

  return {
    mode: "enforce",
    filesystem: {
      denyRead: DENY_READ_DIRS.map((p) => path.join(home, p)),
      allowRead: [],
      allowWrite,
      denyWrite: [".opencode", "opencode.json", "opencode.jsonc", openCodeConfigDir],
    },
    network: {
      allowedDomains: [...ALLOWED_DOMAINS],
      deniedDomains: [],
      allowLocalBinding: false,
    },
  };
}
