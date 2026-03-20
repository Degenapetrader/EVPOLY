import { spawnSync } from "node:child_process";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const cargoBin = path.join(os.homedir(), ".cargo", "bin");
const env = { ...process.env };
const pathKey = process.platform === "win32" ? "Path" : "PATH";
const pathSep = process.platform === "win32" ? ";" : ":";
const currentPath = env[pathKey] ?? "";

if (!currentPath.split(pathSep).includes(cargoBin)) {
  env[pathKey] = currentPath ? `${cargoBin}${pathSep}${currentPath}` : cargoBin;
}

const tauriCli = path.join(
  repoRoot,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.cmd" : "tauri",
);
const args = process.argv.slice(2);
const result =
  process.platform === "win32"
    ? spawnSync(
        "powershell",
        [
          "-NoProfile",
          "-ExecutionPolicy",
          "Bypass",
          "-Command",
          `& '${tauriCli.replace(/'/g, "''")}' ${args
            .map((arg) => `'${arg.replace(/'/g, "''")}'`)
            .join(" ")}`,
        ],
        {
          cwd: repoRoot,
          env,
          stdio: "inherit",
        },
      )
    : spawnSync(tauriCli, args, {
        cwd: repoRoot,
        env,
        stdio: "inherit",
      });

if (result.error) {
  throw result.error;
}

process.exit(result.status ?? 1);
