// Main owns browser discovery and launch targets. The renderer receives only
// stable ids, names and icons, and can never supply an executable or arguments.
import { execFile, spawn } from "node:child_process";
import { access, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execute = promisify(execFile);

// Channels have their own ids because both can be installed at once. Paths
// are relative to the OS's application roots; Linux also checks PATH.
const BROWSERS = [
  { id: "chrome", name: "Google Chrome", mac: "Google Chrome", bundle: "com.google.Chrome", win: "Google/Chrome/Application/chrome.exe", linux: ["google-chrome", "google-chrome-stable"] },
  { id: "firefox", name: "Firefox", mac: "Firefox", bundle: "org.mozilla.firefox", win: "Mozilla Firefox/firefox.exe", linux: ["firefox"] },
  { id: "safari", name: "Safari", mac: "Safari", bundle: "com.apple.Safari" },
  { id: "edge", name: "Microsoft Edge", mac: "Microsoft Edge", bundle: "com.microsoft.edgemac", win: "Microsoft/Edge/Application/msedge.exe", linux: ["microsoft-edge", "microsoft-edge-stable"] },
  { id: "brave", name: "Brave", mac: "Brave Browser", bundle: "com.brave.Browser", win: "BraveSoftware/Brave-Browser/Application/brave.exe", linux: ["brave-browser", "brave"] },
  { id: "opera", name: "Opera", mac: "Opera", bundle: "com.operasoftware.Opera", win: "Programs/Opera/launcher.exe", linux: ["opera"] },
  { id: "vivaldi", name: "Vivaldi", mac: "Vivaldi", bundle: "com.vivaldi.Vivaldi", win: "Vivaldi/Application/vivaldi.exe", linux: ["vivaldi", "vivaldi-stable"] },
  { id: "arc", name: "Arc", mac: "Arc", bundle: "company.thebrowser.Browser" },
  { id: "zen", name: "Zen", mac: "Zen", bundle: "app.zen-browser.zen", win: "Zen Browser/zen.exe", linux: ["zen-browser", "zen"] },
  { id: "orion", name: "Orion", mac: "Orion", bundle: "com.kagi.kagimacOS" },
  { id: "chromium", name: "Chromium", mac: "Chromium", bundle: "org.chromium.Chromium", win: "Chromium/Application/chrome.exe", linux: ["chromium", "chromium-browser"] },
  { id: "chrome-canary", name: "Google Chrome Canary", mac: "Google Chrome Canary", bundle: "com.google.Chrome.canary", win: "Google/Chrome SxS/Application/chrome.exe" },
  { id: "firefox-developer", name: "Firefox Developer Edition", mac: "Firefox Developer Edition", bundle: "org.mozilla.firefoxdeveloperedition", win: "Firefox Developer Edition/firefox.exe", linux: ["firefox-developer-edition"] },
  { id: "safari-preview", name: "Safari Technology Preview", mac: "Safari Technology Preview", bundle: "com.apple.SafariTechnologyPreview" },
];

async function isInstalled(target, bundle, platform) {
  try {
    const info = await stat(target);
    if (bundle) return info.isDirectory() && (await stat(path.join(target, "Contents/Info.plist"))).isFile();
    if (!info.isFile()) return false;
    if (platform !== "win32") await access(target, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

// A new browser process may stay alive for hours. Observe startup errors and
// immediate refusals without waiting for the user to close their browser.
function launchExecutable(executable, args) {
  return new Promise((resolve) => {
    let timer;
    let settled = false;
    const finish = (ok) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(ok);
    };
    const child = spawn(executable, args, { detached: true, stdio: "ignore", shell: false, windowsHide: true });
    child.once("error", () => finish(false));
    child.once("exit", (code) => finish(code === 0));
    child.once("spawn", () => {
      child.unref();
      timer = setTimeout(() => finish(true), 300);
    });
  });
}

export function createInstalledBrowserService({
  platform = process.platform,
  homeDirectory = homedir(),
  env = process.env,
  getIcon = async () => null,
  installed = (target, bundle) => isInstalled(target, bundle, platform),
  run = execute,
  launch = launchExecutable,
} = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  const roots = platform === "darwin"
    ? ["/Applications", paths.join(homeDirectory, "Applications"), "/System/Applications"]
    : platform === "win32"
      ? [env.LOCALAPPDATA, env.ProgramFiles ?? env.PROGRAMFILES, env["ProgramFiles(x86)"] ?? env["PROGRAMFILES(X86)"]].filter(Boolean)
      : [...new Set([...(env.PATH ?? "").split(":"), "/usr/bin", "/usr/local/bin", "/snap/bin"].filter(p => paths.isAbsolute(p)))];
  const icons = new Map();
  let discovering = null;
  let known = new Map();

  async function discover() {
    // Spotlight finds moved/renamed macOS apps too. Standard roots still work
    // when indexing is disabled or its database has not caught up yet.
    const indexed = new Map();
    if (platform === "darwin") {
      try {
        const query = BROWSERS.map(b => `kMDItemCFBundleIdentifier == '${b.bundle}'`).join(" || ");
        const { stdout } = await run("/usr/bin/mdfind", ["-0", query], { timeout: 3000, maxBuffer: 256 * 1024 });
        const targets = [...new Set(stdout.split("\0").filter(p => paths.isAbsolute(p) && p.endsWith(".app")))];
        await Promise.all(targets.map(async (target) => {
          try {
            const { stdout: bundle } = await run("/usr/bin/plutil", ["-extract", "CFBundleIdentifier", "raw", "-o", "-", paths.join(target, "Contents/Info.plist")], { timeout: 1000, maxBuffer: 4096 });
            const id = bundle.trim();
            indexed.set(id, [...(indexed.get(id) ?? []), target]);
          } catch { /* stale metadata */ }
        }));
      } catch { /* standard install locations remain available */ }
    }
    const found = await Promise.all(BROWSERS.map(async (browser) => {
      let candidates;
      if (platform === "darwin") {
        candidates = roots.map(root => paths.join(root, `${browser.mac}.app`));
        // Check bundle identity for indexed paths: a renamed Chrome must not
        // become Firefox merely because both came back from the same query.
        candidates.push(...(indexed.get(browser.bundle) ?? []));
      } else if (platform === "win32") {
        candidates = browser.win ? roots.map(root => paths.join(root, browser.win)) : [];
      } else if (platform === "linux") {
        candidates = (browser.linux ?? []).flatMap(name => roots.map(root => paths.join(root, name)));
      } else {
        candidates = [];
      }
      for (const target of candidates) {
        if (!await installed(target, platform === "darwin")) continue;
        let icon = icons.get(target) ?? null;
        if (icon === null) {
          try { icon = await getIcon(target, browser.id); } catch { /* retain a named fallback */ }
          if (icon) icons.set(target, icon);
        }
        return { id: browser.id, name: browser.name, icon, target };
      }
      return null;
    }));
    known = new Map(found.filter(Boolean).map(browser => [browser.id, browser]));
    return [...known.values()];
  }

  async function list() {
    // Coalesce requests from multiple viewers, but re-scan on the next open
    // or window focus so installs/removals do not need an app restart.
    discovering ??= discover().finally(() => { discovering = null; });
    return (await discovering).map(({ id, name, icon }) => ({ id, name, icon }));
  }

  async function resolve(id) {
    if (typeof id !== "string" || !BROWSERS.some(browser => browser.id === id)) return null;
    if (discovering) await discovering;
    if (!known.has(id)) await list();
    const browser = known.get(id);
    return browser && await installed(browser.target, platform === "darwin") ? browser : null;
  }

  async function open(browser, target) {
    // Only a discovery result from this service is a launch capability.
    if (!browser || known.get(browser.id)?.target !== browser.target) return false;
    const url = pathToFileURL(target).href;
    try {
      if (platform === "darwin") {
        await run("/usr/bin/open", ["-a", browser.target, url], { timeout: 10_000, maxBuffer: 4096 });
        return true;
      }
      return await launch(browser.target, [url]);
    } catch {
      return false;
    }
  }

  return { list, resolve, open };
}
