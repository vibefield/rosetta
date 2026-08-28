// Dev runner: start Vite, wait for it, then launch Electron pointed at it.
// Ctrl-C tears both down.
import { spawn } from "node:child_process";

const PORT = 5185;
const URL_ = `http://localhost:${PORT}`;

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
  env: { ...process.env, ELECTRON_RENDERER_URL: URL_ },
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
