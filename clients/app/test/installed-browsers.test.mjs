import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { createInstalledBrowserService } from "../electron/installed-browsers.mjs";

test("macOS discovers system, user and renamed browser apps, and exposes no launch paths", async () => {
  const apps = new Set(["/Applications/Google Chrome.app", "/Users/test/Applications/Firefox.app", "/Tools/My Safari.app"]);
  const icons = [];
  const launches = [];
  const service = createInstalledBrowserService({
    platform: "darwin", homeDirectory: "/Users/test", env: {},
    installed: async (target, bundle) => { assert.equal(bundle, true); return apps.has(target); },
    getIcon: async target => { icons.push(target); return "data:image/png;base64,icon"; },
    run: async (command, args) => {
      if (command.endsWith("mdfind")) return { stdout: "/Tools/My Safari.app\0/Applications/Google Chrome.app\0" };
      if (command.endsWith("plutil")) return { stdout: args.at(-1).includes("My Safari") ? "com.apple.Safari\n" : "com.google.Chrome\n" };
      launches.push([command, args]);
      return { stdout: "" };
    },
  });
  const browsers = await service.list();
  assert.deepEqual(browsers.map(browser => browser.id), ["chrome", "firefox", "safari"]);
  assert.ok(browsers.every(browser => Object.keys(browser).sort().join() === "icon,id,name"));
  assert.ok(browsers.every(browser => browser.icon.startsWith("data:image/")));

  const target = "/tmp/report #1 ü;$(command).html";
  for (const browser of browsers) assert.equal(await service.open(await service.resolve(browser.id), target), true);
  assert.deepEqual(launches, [...apps].map(app => ["/usr/bin/open", ["-a", app, pathToFileURL(target).href]]));
  await service.list();
  assert.equal(icons.length, 3, "Native icons are reused on focus refreshes");
  apps.delete("/Users/test/Applications/Firefox.app");
  assert.equal(await service.resolve("firefox"), null, "An uninstalled browser cannot launch from a stale button");
  assert.deepEqual((await service.list()).map(browser => browser.id), ["chrome", "safari"]);
});

test("Windows discovers per-user and system installs and launches a file URL as one argument", async () => {
  const chrome = "C:\\Users\\A User\\AppData\\Local\\Google\\Chrome\\Application\\chrome.exe";
  const edge = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
  const launches = [];
  const service = createInstalledBrowserService({
    platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\A User\\AppData\\Local", ProgramFiles: "C:\\Program Files", "ProgramFiles(x86)": "C:\\Program Files (x86)" },
    installed: async target => [chrome, edge].includes(target),
    getIcon: async () => { throw new Error("icon unavailable"); },
    launch: async (...args) => { launches.push(args); return true; },
  });
  assert.deepEqual((await service.list()).map(({ id, icon }) => [id, icon]), [["chrome", null], ["edge", null]]);
  const browser = await service.resolve("edge");
  assert.equal(await service.open(browser, "/tmp/a b#c.html"), true);
  assert.deepEqual(launches, [[edge, [pathToFileURL("/tmp/a b#c.html").href]]]);
  assert.equal(await service.open({ ...browser, target: "C:\\evil.exe" }, "/tmp/a.html"), false);
  for (const id of [null, {}, "C:\\evil.exe", "chrome --no-sandbox", "__proto__"]) assert.equal(await service.resolve(id), null);
  assert.equal(launches.length, 1);
});

test("Linux requires an executable, ignores relative PATH entries, and handles startup refusal", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "rosetta-browsers-"));
  try {
    const firefox = path.join(directory, "firefox");
    await writeFile(firefox, "#!/bin/sh\nexit 1\n", { mode: 0o600 });
    const service = createInstalledBrowserService({ platform: "linux", env: { PATH: `.:${directory}` } });
    await service.list();
    assert.notEqual((await service.resolve("firefox"))?.target, firefox);
    await chmod(firefox, 0o700);
    assert.equal((await service.list()).some(browser => browser.id === "firefox"), true);
    assert.equal(await service.open(await service.resolve("firefox"), "/tmp/file.html"), false);
  } finally { await rm(directory, { force: true, recursive: true }); }
});

test("missing Spotlight, no installed browsers, and OS launch failures remain honest", async () => {
  let present = false;
  const service = createInstalledBrowserService({
    platform: "darwin", homeDirectory: "/Users/test", env: {},
    installed: async target => present && target === "/Applications/Safari.app",
    run: async () => { throw new Error("OS refused"); },
  });
  assert.deepEqual(await service.list(), []);
  assert.equal(await service.resolve("safari"), null);
  present = true;
  assert.deepEqual((await service.list()).map(browser => browser.id), ["safari"]);
  assert.equal(await service.open(await service.resolve("safari"), "/tmp/file.html"), false);
});
