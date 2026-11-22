// @ts-check

/**
 * @typedef {{ command: string, shell: string, env: Record<string, string | undefined> }} ShellEvent
 */

/**
 * Builds the shell create.before hook. The hook wraps every shell command with the
 * sandbox and sets SUPERPOWERS_DISABLE_TELEMETRY=1 in the env. It does not read or
 * set GH_TOKEN: on OpenCode, gh goes only through the gh tool.
 *
 * The OpenCode callback type returns void, so the env change is made on the event
 * object, which OpenCode then uses.
 * @param {{ wrap: (event: ShellEvent) => Promise<void> }} sandbox
 * @returns {(event: ShellEvent) => Promise<void>}
 */
export function createShellHook(sandbox) {
  return async function shellCreateBefore(event) {
    await sandbox.wrap(event);
    event.env.SUPERPOWERS_DISABLE_TELEMETRY = "1";
  };
}
