// Dev runner: build the current daemon with production optimizations, start
// Vite, then launch Electron with that exact binary. Selecting whichever
// debug/release artifact happened to have the newest mtime made the UI's cold
// path both stale and 10x slower than the measured pipeline.
import { execFile, spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 5185;
const URL_ = `http://localhost:${PORT}`;
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.join(here, "..", "..", "..");

function waitForExit(child, label) {
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`${label} exited with ${signal ?? code}`));
    });
  });
}

function cargoMetadata() {
  return new Promise((resolve, reject) => {
    execFile(
      "cargo",
      ["metadata", "--format-version", "1", "--no-deps"],
      { cwd: repo, timeout: 10000, maxBuffer: 16 * 1024 * 1024 },
      (error, stdout) => {
        if (error) reject(error);
        else {
          try {
            resolve(JSON.parse(stdout));
          } catch (parseError) {
            reject(parseError);
          }
        }
      },
    );
  });
}

async function optimizedDaemon() {
  if (process.env.ROSETTAD_BIN) return process.env.ROSETTAD_BIN;
  console.log("building optimized rosettad for the app…");
  const cargo = spawn("cargo", ["build", "--release", "-p", "rosettad"], {
    cwd: repo,
    stdio: "inherit",
  });
  await waitForExit(cargo, "cargo build --release -p rosettad");
  const metadata = await cargoMetadata();
  return path.join(metadata.target_directory, "release", "rosettad");
}

let daemonBin;
try {
  daemonBin = await optimizedDaemon();
} catch (error) {
  console.error(`could not prepare rosettad: ${error.message}`);
  process.exit(1);
}

const vite = spawn("npx", ["vite"], { stdio: "inherit" });

async function waitForVite() {
  for (let i = 0; i < 120; i += 1) {
    try {
      const res = await fetch(URL_);
      if (res.ok) return true;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

const up = await waitForVite();
if (!up) {
  console.error(`vite never answered on ${URL_}`);
  vite.kill();
  process.exit(1);
}

const electron = spawn("npx", ["electron", "."], {
  stdio: "inherit",
  env: { ...process.env, ELECTRON_RENDERER_URL: URL_, ROSETTAD_BIN: daemonBin },
});

function stop() {
  electron.kill();
  vite.kill();
}
electron.on("exit", () => {
  vite.kill();
  process.exit(0);
});
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
