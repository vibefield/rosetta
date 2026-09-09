// Run with `npm run test:ui-motion`. Uses a disposable Electron profile and
// real React/CSS/layout; never launches a daemon or reads the user's archive.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-motion-"));
app.setPath("userData", path.join(temporary, "profile"));
// Keep Node's ESM launch awake until Electron's native ready event.
const launchPulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => {
  console.error("UI motion test timed out");
  process.exit(1);
}, 90000);
let server;
let win;
let failed = false;
const errors = [];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const check = (fn, ...args) =>
  win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) {
    if (await check(fn)) return;
    await pause(25);
  }
  throw new Error(message);
}
const atBottom = () => {
  const s = document.querySelector(".tr-scroll");
  return s && Math.abs(s.scrollHeight - s.clientHeight - s.scrollTop) < 2;
};
const buttonVisible = () => document.querySelector(".ah-newpill")?.hasAttribute("data-visible");

async function run() {
  try {
    await app.whenReady();
    clearInterval(launchPulse);
    console.log("Electron ready; starting the isolated UI fixture.");
    const { createServer } = await import("vite");
    server = await createServer({
      configFile: path.join(root, "vite.config.ts"),
      root,
      logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false },
    });
    await server.listen();
    const port = server.httpServer.address().port;
    win = new BrowserWindow({
      width: 1320,
      height: 860,
      show: false,
      webPreferences: {
        backgroundThrottling: false,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    win.webContents.on("console-message", (event) => {
      if (event.level === "error") errors.push(event.message);
    });
    win.webContents.on("render-process-gone", (_event, details) =>
      errors.push(JSON.stringify(details)),
    );
    await win.loadURL(`http://127.0.0.1:${port}/test/ui-motion.html`);
    await until(
      () => Boolean(window.motionFixture && document.querySelectorAll(".tr-prose").length > 5),
      "fixture did not render",
    );
    await until(atBottom, "initial session did not land at the foot");
    console.log("Fixture rendered; checking motion and scroll behavior.");

    const activity = await check(() => ({
      old: document.querySelector('[data-motion-key="s7"] .ah-last').textContent,
      status: document.querySelector('[data-motion-key="s0"] .ah-session-list-item__status').title,
      days: [...document.querySelectorAll(".ah-session-list .ah-day")].map(node => node.textContent),
      falseRunning: document.querySelector('[title="this session is still running"]') !== null,
    }));
    assert.equal(activity.old, "yesterday", "a fresh metadata date must not make an old conversation now");
    assert.equal(activity.status, "history incomplete");
    assert.deepEqual(activity.days, ["Today", "Yesterday"]);
    assert.equal(activity.falseRunning, false);

    await check(() => document.querySelector('[aria-label="Tasks, 2"]').click());
    await until(() => Boolean(document.querySelector('[aria-label="Full task list"]')), "task panel did not open");
    assert.deepEqual(await check(() => [...document.querySelectorAll(".tr-meta-row__value")].map(node => node.textContent)), ["—", "1m 00s"]);
    await check(() => [...document.querySelectorAll('.tr-meta-tab')].find(node => node.textContent === "Workflows").click());
    await until(() => Boolean(document.querySelector('[aria-label="Workflow runs"]')), "workflow panel did not open");
    assert.equal(await check(() => document.querySelector(".tr-meta-row__value").textContent), "—");
    await check(() => document.querySelector('[aria-label="Close session work"]').click());
    await until(() => !document.querySelector('.tr-meta-popover'), "work panel did not close");

    const reorder = await check(() => {
      const target = document.querySelector('[data-motion-key="p5"]');
      const before = target.getBoundingClientRect().top;
      window.motionFixture.flush(window.motionFixture.reorder);
      return {
        before,
        after: target.getBoundingClientRect().top,
        animations: target.getAnimations().length,
        first: document.querySelector(".ah-project-list-item").dataset.motionKey,
      };
    });
    assert.equal(reorder.first, "p5");
    assert.ok(reorder.animations > 0, "reordered project must animate");
    assert.ok(
      Math.abs(reorder.before - reorder.after) < 2,
      "reorder must start at its previous visual position",
    );
    await pause(80);
    const retarget = await check(() => {
      const target = document.querySelector('[data-motion-key="p5"]');
      const before = target.getBoundingClientRect().top;
      window.motionFixture.flush(window.motionFixture.reorder);
      return Math.abs(before - target.getBoundingClientRect().top);
    });
    assert.ok(retarget < 2, "rapid sorting must retarget without snapping");

    assert.equal(
      await check(() => {
        const target = document.querySelector('[data-motion-key="s7"]');
        target.focus({ preventScroll: true });
        window.motionFixture.flush(window.motionFixture.promote);
        return (
          document.activeElement === target &&
          document.querySelector(".ah-session-list-item") === target &&
          target.getAnimations().length > 0
        );
      }),
      true,
      "crossing day groups must retain the button and its keyboard focus",
    );

    const appended = await check(() => {
      const s = document.querySelector(".tr-scroll");
      const before = s.scrollTop;
      window.motionFixture.flush(() => window.motionFixture.append(2));
      const last = [...document.querySelectorAll("[id][data-timeline-id]")].at(-1);
      return {
        before,
        after: s.scrollTop,
        gap: s.scrollHeight - s.clientHeight - s.scrollTop,
        animations: last.getAnimations().length,
      };
    });
    assert.equal(appended.before, appended.after, "append must not jump");
    assert.ok(appended.gap > 40);
    assert.ok(appended.animations > 0, "new message must have an entrance transition");
    await until(atBottom, "append follow did not settle");

    await check(() => {
      const s = document.querySelector(".tr-scroll");
      s.dispatchEvent(new WheelEvent("wheel", { deltaY: -300, bubbles: true }));
      s.scrollTop -= 300;
    });
    await until(buttonVisible, "Latest must appear even without unread messages");
    assert.equal(await check(() => document.querySelector(".ah-newpill").textContent), "Latest");
    const reading = await check(() => {
      const s = document.querySelector(".tr-scroll");
      const before = s.scrollTop;
      window.motionFixture.flush(() => window.motionFixture.append(5));
      window.motionFixture.flush(window.motionFixture.grow);
      return {
        before,
        after: s.scrollTop,
        label: document.querySelector(".ah-newpill").textContent,
      };
    });
    assert.equal(reading.before, reading.after, "paused reader must stay in place");
    assert.equal(reading.label, "5 new messages", "streaming patches must not increment the count");
    await pause(80); // allow the history hook to cache the visible anchor
    const history = await check(() => {
      const s = document.querySelector(".tr-scroll");
      const edge = s.getBoundingClientRect().top;
      const anchor = [...s.querySelectorAll("[id][data-timeline-id]")].find(
        (el) => el.getBoundingClientRect().bottom > edge,
      );
      const before = anchor.getBoundingClientRect().top;
      window.motionFixture.flush(window.motionFixture.prepend);
      return {
        delta: Math.abs(before - document.getElementById(anchor.id).getBoundingClientRect().top),
        label: document.querySelector(".ah-newpill").textContent,
      };
    });
    assert.ok(history.delta < 2, "history prepend must preserve the visible message");
    assert.equal(history.label, "5 new messages");
    await pause(300);
    const screenshot = path.join(temporary, "unread-light.png");
    await fs.writeFile(screenshot, (await win.webContents.capturePage()).toPNG());

    await check(() => document.querySelector(".ah-newpill").click());
    await pause(60);
    assert.equal(
      await check(atBottom),
      false,
      "return to latest should traverse intermediate frames",
    );
    await check(() =>
      document
        .querySelector(".tr-scroll")
        .dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true })),
    );
    await until(buttonVisible, "wheel input must interrupt return to latest");
    assert.equal(
      await check(() => document.querySelector(".ah-newpill").textContent),
      "5 new messages",
    );
    await check(() => document.querySelector(".ah-newpill").click());
    await until(atBottom, "return to latest did not settle");
    assert.equal(await check(buttonVisible), false);
    assert.equal(
      await check(() => document.activeElement === document.querySelector(".tr-scroll")),
      true,
    );
    win.webContents.sendInputEvent({ type: "keyDown", keyCode: "PageUp" });
    win.webContents.sendInputEvent({ type: "keyUp", keyCode: "PageUp" });
    await until(buttonVisible, "keyboard scrolling must reveal Latest");
    await check(() => document.querySelector(".ah-newpill").click());
    await until(atBottom, "keyboard reader must be able to return to the foot");

    const grew = await check(() => {
      const s = document.querySelector(".tr-scroll");
      const before = s.scrollTop;
      window.motionFixture.flush(window.motionFixture.grow);
      return { before, after: s.scrollTop };
    });
    assert.equal(grew.before, grew.after, "content resize must not jump");
    await until(atBottom, "ResizeObserver did not follow growing content");
    win.setSize(1120, 720);
    await until(atBottom, "window resize must retain live-follow");
    win.setSize(1320, 860);
    await until(atBottom, "restoring window size must retain live-follow");

    await check(() => window.motionFixture.seek());
    await until(buttonVisible, "search landing must suspend follow");
    await check(() => window.motionFixture.switchSession());
    await until(atBottom, "switching session must reset follow and unread state");
    assert.equal(await check(buttonVisible), false);

    win.webContents.debugger.attach("1.3");
    await win.webContents.debugger.sendCommand("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-reduced-motion", value: "reduce" }],
    });
    await until(
      () => matchMedia("(prefers-reduced-motion: reduce)").matches,
      "media emulation failed",
    );
    const reducedState = await check(async () => {
      const followingBefore = document.querySelector(".tr-scroll").hasAttribute("data-following");
      window.motionFixture.flush(window.motionFixture.reorder);
      window.motionFixture.flush(() => window.motionFixture.append(2));
      // Message previews can finish measuring after the first React commit.
      // Allow ResizeObserver's layout pass; reduced motion must still reach
      // the exact foot without starting any movement animation.
      await new Promise(requestAnimationFrame);
      await new Promise(requestAnimationFrame);
      const s = document.querySelector(".tr-scroll");
      return {
        followingBefore,
        gap: s.scrollHeight - s.clientHeight - s.scrollTop,
        animations: [...document.querySelectorAll("[data-motion-key]")].flatMap((row) =>
          row
            .getAnimations()
            .filter((animation) =>
              animation.effect.getKeyframes().some((frame) => frame.transform !== undefined),
            )
            .map(() => row.dataset.motionKey),
        ),
      };
    });
    assert.ok(
      reducedState.gap < 2 && reducedState.animations.length === 0,
      `reduced motion must skip list and scroll movement: ${JSON.stringify(reducedState)}`,
    );

    await check(() => {
      document.documentElement.classList.add("dark");
      document.documentElement.setAttribute("data-theme", "dark");
      const s = document.querySelector(".tr-scroll");
      s.scrollTop -= 350;
    });
    await until(buttonVisible, "dark screenshot must show the latest control");
    await check(() => window.motionFixture.append(3));
    await pause(60);
    const darkScreenshot = path.join(temporary, "unread-dark.png");
    await fs.writeFile(darkScreenshot, (await win.webContents.capturePage()).toPNG());
    assert.deepEqual(errors, [], "renderer errors");
    console.log(
      JSON.stringify(
        {
          result: "PASS",
          checked: [
            "reorder",
            "interruptible motion",
            "day-group focus",
            "message entrance",
            "smooth follow",
            "reading position",
            "unread count",
            "history anchor",
            "return to latest",
            "dynamic growth",
            "keyboard scrolling",
            "window resizing",
            "search landing",
            "session reset",
            "reduced motion",
          ],
          screenshots: [screenshot, darkScreenshot],
        },
        null,
        2,
      ),
    );
  } catch (error) {
    failed = true;
    console.error(error);
    if (errors.length) console.error(errors);
  } finally {
    clearInterval(launchPulse);
    clearTimeout(deadline);
    win?.destroy();
    await server?.close();
    app.exit(failed ? 1 : 0);
  }
}
// Electron emits ready after the entry module finishes evaluating. Awaiting
// it at module scope would deadlock that very event.
void run();
