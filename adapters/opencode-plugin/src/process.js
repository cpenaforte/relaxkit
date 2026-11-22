// @ts-check
import { spawn as nodeSpawn } from "node:child_process";

/**
 * @typedef {typeof nodeSpawn} SpawnFn
 * @typedef {{ code: number | null, stdout: string, stderr: string }} RunResult
 */

/**
 * Runs a binary without a shell and collects its output. A spawn error (for
 * example, the binary is missing) resolves with a null code and the error
 * message in stderr.
 * @param {SpawnFn} spawn
 * @param {string} command
 * @param {string[]} args
 * @param {import("node:child_process").SpawnOptions} options
 * @returns {Promise<RunResult>}
 */
export function runProcess(spawn, command, args, options) {
  return new Promise((resolve) => {
    let stdout = "";
    let stderr = "";
    const child = spawn(command, args, options);
    child.stdout?.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", (error) => {
      resolve({ code: null, stdout, stderr: stderr + error.message });
    });
    child.on("close", (code) => {
      resolve({ code, stdout, stderr });
    });
  });
}

/**
 * The text shown for a run that did not exit with status 0: stderr, then the exit code.
 * @param {RunResult} result
 * @returns {string}
 */
export function formatFailure(result) {
  const code = result.code === null ? "none" : String(result.code);
  return [result.stderr.trimEnd(), `exit code: ${code}`].filter(Boolean).join("\n");
}
