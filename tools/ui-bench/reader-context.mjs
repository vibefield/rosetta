// The reader scripts' one door to the page, in the Electron main process. Every
// input goes through `input()`, which dispatches a trusted CDP Input event and stamps
// it; every wait is on a probe event or a page state (never on a wall-clock aim);
// targets are asked of the probe as rectangles, so no id ever reaches this process.
const KEYS = {
  Escape: { key: 'Escape', code: 'Escape', vk: 27 },
  Enter: { key: 'Enter', code: 'Enter', vk: 13, text: '\r' },
  Backspace: { key: 'Backspace', code: 'Backspace', vk: 8, commands: ['deleteBackward'] },
  ' ': { key: ' ', code: 'Space', vk: 32, text: ' ' },
  _: { key: '_', code: 'Minus', vk: 189, text: '_', shift: true },
  '-': { key: '-', code: 'Minus', vk: 189, text: '-' },
};
function keyDef(ch) {
  if (KEYS[ch]) return KEYS[ch];
  if (/^[a-z]$/.test(ch)) return { key: ch, code: `Key${ch.toUpperCase()}`, vk: ch.toUpperCase().charCodeAt(0), text: ch };
  if (/^[A-Z]$/.test(ch)) return { key: ch, code: `Key${ch}`, vk: ch.charCodeAt(0), text: ch, shift: true };
  if (/^[0-9]$/.test(ch)) return { key: ch, code: `Digit${ch}`, vk: ch.charCodeAt(0), text: ch };
  throw new Error('reader-context: no key definition for a character in the script');
}
const META = 4;
const SHIFT = 8;

export function createReaderContext({ dbg, events, watchers, mainNow, log, enroll }) {
  const inputs = [];
  const marks = [];
  let pointer = { x: 20, y: 20 };
  const cdp = (method, params) => dbg.sendCommand(method, params);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  async function q(op, arg) {
    const expression = `window.__uiBench ? window.__uiBench.q(${JSON.stringify(op)}, ${JSON.stringify(arg ?? null)}) : undefined`;
    const r = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    return r.exceptionDetails ? null : (r.result?.value ?? null);
  }
  const count = (name) => events.get(name)?.count ?? 0;
  function waitFor(test, timeoutMs) {
    return new Promise((resolve) => {
      const hit = test();
      if (hit !== null) { resolve(hit); return; }
      let timer = null;
      const check = () => {
        const value = test();
        if (value === null) return;
        watchers.delete(check); clearTimeout(timer); resolve(value);
      };
      watchers.add(check);
      timer = setTimeout(() => { watchers.delete(check); resolve(null); }, timeoutMs);
    });
  }
  /** Resolves with the probe event once it has fired `count` times, or null. */
  const until = (name, { count: n = 1, timeoutMs = 120_000 } = {}) =>
    waitFor(() => (count(name) >= n ? events.get(name).last : null), timeoutMs);
  /** specs: [[name, count], ...]; resolves with the first name satisfied, or null. */
  const untilAny = (specs, timeoutMs = 120_000) =>
    waitFor(() => { for (const [name, n] of specs) if (count(name) >= (n ?? 1)) return name; return null; }, timeoutMs);

  async function input(kind, method, params) {
    inputs.push([mainNow(), kind]);
    await cdp(method, params);
  }
  async function move(x, y, steps = 10, overMs = 250) {
    const from = pointer;
    for (let i = 1; i <= steps; i += 1) {
      const px = from.x + ((x - from.x) * i) / steps;
      const py = from.y + ((y - from.y) * i) / steps;
      await input('move', 'Input.dispatchMouseEvent', { type: 'mouseMoved', x: px, y: py, button: 'none', buttons: 0 });
      await sleep(overMs / steps);
    }
    pointer = { x, y };
  }
  async function press() {
    const { x, y } = pointer;
    await input('down', 'Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', buttons: 1, clickCount: 1 });
    await input('up', 'Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', buttons: 0, clickCount: 1 });
  }
  async function wheel(x, y, deltaY) {
    await input('wheel', 'Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: 0, deltaY });
    pointer = { x, y };
  }
  /** Read the target's rect, travel to it in 10 steps over 250 ms, click.
   * Returns { ok, aimId }; the probe decides whether the click hit (H7 c). */
  async function aim(spec, { click = true } = {}) {
    let r = await q('rect', spec);
    for (let i = 0; r && !r.visible && r.dir !== 0 && r.sx !== null && i < 20; i += 1) {
      await wheel(r.sx, r.sy, r.dir * 240);
      await sleep(150);
      r = await q('rect', spec);
    }
    if (!r || !r.visible) { mark(`absent:${spec.split(':')[0]}`); return { ok: false, aimId: null }; }
    await move(r.x + r.w / 2, r.y + Math.min(r.h / 2, 16));
    if (click) await press();
    return { ok: true, aimId: r.aimId };
  }
  async function key(ch, { meta = false } = {}) {
    const d = keyDef(ch);
    const modifiers = (meta ? META : 0) | (d.shift ? SHIFT : 0);
    const base = { key: d.key, code: d.code, windowsVirtualKeyCode: d.vk, nativeVirtualKeyCode: d.vk, modifiers };
    const text = !meta && d.text ? { text: d.text, unmodifiedText: d.text } : {};
    const commands = d.commands ? { commands: d.commands } : {};
    await input(meta ? 'shortcut' : 'key', 'Input.dispatchKeyEvent', { type: meta || !d.text ? 'rawKeyDown' : 'keyDown', ...base, ...text, ...commands });
    await input('key-up', 'Input.dispatchKeyEvent', { type: 'keyUp', ...base });
  }
  async function type(text, perKeyMs = 10) {
    for (const ch of text) { await key(ch); await sleep(perKeyMs); }
  }
  function mark(name) {
    marks.push([mainNow(), name]);
    void q('mark', name);
  }
  async function untilLoaded(timeoutMs = 30_000) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      try { if ((await q('ping')) !== null) return true; } catch { /* navigating */ }
      await sleep(50);
    }
    throw new Error('the probe never answered');
  }
  /** Min-RTT renderer clock offset (renderer − main), over n round trips. */
  async function pingRenderer(n) {
    let best = null;
    const all = [];
    for (let i = 0; i < n; i += 1) {
      const a = mainNow();
      const r = await q('ping');
      const b = mainNow();
      if (typeof r !== 'number') continue;
      const s = { offsetMs: r - (a + b) / 2, rttMs: b - a };
      all.push(s.offsetMs);
      if (best === null || s.rttMs < best.rttMs) best = s;
    }
    return best === null ? null : { ...best, n: all.length };
  }
  return { q, until, untilAny, count, aim, move, press, key, type, sleep, mark, untilLoaded, pingRenderer, log, enroll,
    inputLog: () => inputs.slice(), marks: () => marks.slice() };
}
