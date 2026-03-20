import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const force = process.argv.includes("--force");
const isWindows = process.platform === "win32";

const command = isWindows ? "powershell" : "bash";
const scriptPath = isWindows
  ? path.join(scriptDir, "build-sidecar-windows.ps1")
  : path.join(scriptDir, "build-sidecar-linux.sh");
const args = isWindows
  ? ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", scriptPath]
  : [scriptPath];

if (force) {
  args.push(isWindows ? "-Force" : "--force");
}

const result = spawnSync(command, args, {
  cwd: repoRoot,
  stdio: "inherit",
  env: process.env,
});

if (result.error) {
  throw result.error;
}

process.exit(result.status ?? 1);
