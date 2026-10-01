// Real streamgraph and dialog checks in Electron, using no daemon.
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-models-"));
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Models chart test timed out"); app.exit(1); }, 60000);
const pause = (ms = 80) => new Promise(resolve => setTimeout(resolve, ms));
let server, win, failed = false;
const errors = [];
const check = async (fn, ...args) => {
  try { return await win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`); }
  catch (error) { throw new Error(`Renderer check failed: ${fn.toString()}`, { cause: error }); }
};
async function until(fn, message) {
  for (let index = 0; index < 100; index++) { if (await check(fn)) return; await pause(30); }
  throw new Error(message);
}
async function point(selector) {
  return check(selector => {
    const rect = document.querySelector(selector).getBoundingClientRect();
    return { x: Math.round(rect.x + rect.width / 2), y: Math.round(rect.y + rect.height / 2) };
  }, selector);
}
async function clickAt(position) {
  win.webContents.sendInputEvent({ type: "mouseMove", ...position });
  win.webContents.sendInputEvent({ type: "mouseDown", button: "left", clickCount: 1, ...position });
  win.webContents.sendInputEvent({ type: "mouseUp", button: "left", clickCount: 1, ...position });
  await pause();
}
const click = async selector => clickAt(await point(selector));
async function key(keyCode, modifiers = []) {
  // Resizing a click-through test window can release native focus on macOS.
  // Real Tab traversal needs browser focus-in events, not only key handlers.
  if (!await check(() => document.hasFocus())) {
    app.focus({ steal: true }); win.focus(); win.webContents.focus();
    await until(() => document.hasFocus(), 'Fixture did not regain native keyboard focus');
  }
  win.webContents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
  if (keyCode === "Enter") win.webContents.sendInputEvent({ type: "char", keyCode: "\r", modifiers });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
  await pause();
}
async function shot(name) { await pause(250); await fs.writeFile(path.join(temporary, name), (await win.webContents.capturePage()).toPNG()); }
const opener = '.ah-chart-launcher--models .ah-chart-launcher__button';
async function open() { await click(opener); await until(() => Boolean(document.querySelector('.ah-models')), "Models dialog did not open"); }
async function inspect(index) {
  // Controlled DOM pointer events use the actual rendered plot coordinates,
  // without the macOS desktop cursor replacing an inspected sample.
  await check(index => {
    const svg = document.querySelector('.ah-models svg'), rect = svg.getBoundingClientRect();
    svg.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, pointerType: 'mouse',
      clientX: Math.max(rect.left + 1, Math.min(rect.right - 1, rect.x + rect.width * index / 29)),
      clientY: rect.y + rect.height / 2 }));
  }, index);
  await pause();
}
async function assertReadoutFits() {
  assert.ok(await check(() => {
    const plot = document.querySelector('.ah-models svg').getBoundingClientRect();
    const readout = document.querySelector('.ah-streamgraph__readout').getBoundingClientRect();
    const body = document.querySelector('.ah-models');
    return readout.left >= plot.left && readout.right <= plot.right && readout.top >= plot.top
      && readout.bottom <= plot.bottom && body.scrollHeight <= body.clientHeight + 1 && body.scrollWidth <= body.clientWidth + 1;
  }), 'The hover card stays inside the plot without scrollbars');
}
async function assertHeaderFits() {
  assert.ok(await check(() => {
    const dialog = document.querySelector('[role="dialog"]');
    const tabs = document.querySelector('[role="tablist"]').getBoundingClientRect();
    const head = document.querySelector('.ah-project-chart__head').getBoundingClientRect();
    const title = document.querySelector('.ah-fileview__title').getBoundingClientRect();
    const actions = document.querySelector('.ah-fileview__acts').getBoundingClientRect();
    const sameRow = tabs.top < title.bottom;
    return Math.abs((tabs.left + tabs.right) / 2 - (head.left + head.right) / 2) < 1
      && tabs.left >= head.left && tabs.right <= head.right && actions.right <= head.right
      && title.right <= (sameRow ? tabs.left : actions.left)
      && (!sameRow || tabs.right <= actions.left) && dialog.scrollWidth <= dialog.clientWidth + 1;
  }), 'Tabs stay centered, with no overlap or overflow in the header');
}

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false } });
    await server.listen();
    win = new BrowserWindow({ width: 1320, height: 900, show: false,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/models-chart.html?tab=models`);
    await until(() => Boolean(window.modelsFixture && document.querySelector('.ah-chart-launcher--models')), 'Fixture did not render');
    await check(() => {
      // Modal clicks and keyboard input still use Electron's native input.
      for (const type of ['pointermove', 'pointerover', 'pointerout']) {
        document.addEventListener(type, event => { if (event.isTrusted) event.stopImmediatePropagation(); }, true);
      }
    });
    assert.match(await check(() => document.querySelector('.ah-streamgraph__legend').textContent), /other ×6/);
    assert.ok(await check(() => document.querySelector('.ah-streamgraph__plot').getBoundingClientRect().height > 30), 'The mini chart retains its plot height');
    const mini = await check(() => ({
      paths: [...document.querySelectorAll('[data-streamgraph-layer]')].map(node => ({
        id: node.dataset.streamgraphLayer, path: node.getAttribute('d'), color: node.getAttribute('fill'),
      })),
      labels: document.querySelector('.ah-streamgraph__axis').textContent,
      legend: document.querySelector('.ah-streamgraph__legend').textContent,
      size: document.querySelector('.ah-streamgraph svg').getAttribute('viewBox'),
    }));
    win.webContents.sendInputEvent({ type: 'mouseMove', ...await point(opener) });
    await pause(200);
    assert.match(await check(() => getComputedStyle(document.querySelector('.ah-chart-launcher--models button')).cursor), /svg.*16 16, zoom-in/);
    await shot('models-mini.png');
    await open();
    assert.equal(await check(() => document.querySelector('[role="tab"][aria-selected="true"]').textContent), 'Models', 'The streamgraph launcher opens Models');
    assert.equal(await check(() => document.querySelector('.ah-activity')), null, 'Opening Models does not mount hourly activity');
    win.show(); app.focus({ steal: true }); win.focus();
    // Keep the physical cursor from replacing the test's injected hover events.
    win.setIgnoreMouseEvents(true);
    await until(() => document.hasFocus(), 'Fixture did not receive native focus');
    const expanded = await check(() => ({
      paths: [...document.querySelectorAll('.ah-models [data-streamgraph-layer]')].map(node => {
        const gradient = document.getElementById(node.getAttribute('fill').slice(5, -1));
        return { id: node.dataset.streamgraphLayer, path: node.getAttribute('d'),
          color: gradient.querySelector('stop').getAttribute('stop-color') };
      }),
      labels: document.querySelector('.ah-models .ah-streamgraph__axis').textContent,
      legend: document.querySelector('.ah-models .ah-streamgraph__legend').textContent,
      size: document.querySelector('.ah-models svg').getAttribute('viewBox'),
    }));
    assert.equal(expanded.labels, mini.labels, 'Both sizes show the same 30 days');
    assert.equal(expanded.size, mini.size, 'The expanded plot uses the same streamgraph layout');
    assert.deepEqual(expanded.paths.map(layer => layer.id).sort(), await check(() => window.modelsFixture.ids.toSorted()),
      'The detailed chart exposes every model behind the mini chart’s other group');
    assert.equal(expanded.paths.length, 9);
    assert.ok(expanded.paths.every(layer => layer.path.includes('C')), 'Every model retains a smooth streamgraph band');
    for (const layer of mini.paths.slice(0, 3)) {
      assert.equal(expanded.paths.find(model => model.id === layer.id).color, layer.color, 'The leading models keep their colors');
    }
    assert.doesNotMatch(expanded.legend, /other ×|legacy-model/, 'The detailed legend has individual models from this period');
    const legend = await check(() => [...document.querySelectorAll('.ah-models [data-streamgraph-series]')].map(node => ({
      id: node.dataset.streamgraphSeries, total: Number(node.querySelector('b').title.replaceAll(',', '')),
      expected: window.modelsFixture.rows.filter(row => row.model === node.dataset.streamgraphSeries).reduce((sum, row) => sum + row.tokens, 0),
    })));
    for (const model of legend) assert.equal(model.total, model.expected, 'Each model has its own accurate period total');
    assert.equal(await check(() => document.querySelectorAll('.ah-models svg').length), 1, 'The popup is one original streamgraph');
    assert.equal(await check(() => document.querySelectorAll('.ah-models button').length), 0, 'No extra measures, model views or drill-down controls');
    assert.ok(await check(() => [...document.querySelectorAll('.ah-models linearGradient')].every(node => {
      const stops = node.querySelectorAll('stop');
      return Number(stops[0].getAttribute('stop-opacity')) > Number(stops[1].getAttribute('stop-opacity'));
    })), 'Gradient fills fade toward the lower edge');
    assert.ok(await check(() => getComputedStyle(document.querySelector('.ah-streamgraph__glow')).filter.includes('blur')), 'Subtle glowing outlines');
    await pause(350);
    await assertHeaderFits();
    await shot('models-streamgraph-dark.png');
    await click('.ah-models svg');
    assert.equal(await check(() => window.modelsFixture.requests.length), 0, 'Enlarging and clicking do not fetch or open another chart');
    await inspect(24);
    assert.equal(await check(() => document.querySelector('.ah-models svg').dataset.inspectedPoint), '2026-09-25');
    assert.match(await check(() => document.querySelector('.ah-streamgraph__readout-head').textContent), /Fri, Sep 25, 2026.*Total.*tokens/);
    const breakdown = await check(() => {
      const rows = [...document.querySelectorAll('[data-point-series]')];
      const total = window.modelsFixture.dailyTotals[24];
      return {
        total: Number(document.querySelector('[data-point-total]').dataset.pointTotal), expectedTotal: total,
        rows: rows.map(node => {
          const id = node.dataset.pointSeries;
          const expected = window.modelsFixture.rows.filter(row => row.day === '2026-09-25' && row.model === id)
            .reduce((sum, row) => sum + row.tokens, 0);
          return { actual: Number(node.querySelector('[data-point-value]').dataset.pointValue), expected,
            share: node.querySelector('small').textContent, expectedShare: `${(expected / total * 100).toFixed(1)}%` };
        }),
      };
    });
    assert.equal(breakdown.total, breakdown.expectedTotal);
    assert.equal(breakdown.rows.length, 9, 'Hover names every model individually');
    assert.equal(breakdown.rows.reduce((sum, row) => sum + row.actual, 0), breakdown.total);
    for (const row of breakdown.rows) { assert.equal(row.actual, row.expected); assert.equal(row.share, row.expectedShare); }
    assert.equal(await check(() => document.querySelectorAll('[data-streamgraph-point]').length), 9);
    assert.ok(await check(() => {
      const svg = document.querySelector('.ah-models svg'), rect = svg.getBoundingClientRect();
      const guide = document.querySelector('.ah-streamgraph__crosshair');
      const x = rect.left + Number(guide.getAttribute('x1')) / 276 * rect.width;
      return [...document.querySelectorAll('[data-streamgraph-point]')].every(node => {
        const marker = node.getBoundingClientRect();
        return Math.abs(marker.left + marker.width / 2 - x) < 1;
      });
    }), 'Guide and markers align on the same recorded sample');
    await assertReadoutFits();
    await shot('models-hover-dark.png');
    await inspect(24.2);
    assert.equal(await check(() => document.querySelector('.ah-streamgraph__readout').dataset.point), '2026-09-25', 'Inspection snaps to days rather than inventing interpolated values');
    await inspect(5);
    assert.equal(await check(() => document.querySelector('[data-point-total]').dataset.pointTotal), '0');
    assert.match(await check(() => document.querySelector('.ah-streamgraph__readout').textContent), /Sep 6, 2026.*No activity recorded/);
    assert.ok(await check(() => [...document.querySelectorAll('[data-point-value]')].every(node => node.dataset.pointValue === '0')));
    assert.ok(await check(() => [...document.querySelectorAll('.ah-streamgraph__readout-rows small')].every(node => node.textContent === '—')));
    assert.equal(await check(() => document.querySelectorAll('[data-streamgraph-point]').length), 0, 'Zero days have no model markers');
    await check(() => document.querySelector('.ah-models svg').dispatchEvent(new PointerEvent('pointerout', {
      bubbles: true, pointerType: 'mouse', relatedTarget: document.querySelector('[aria-label="Close project charts"]'),
    }))); await pause();
    assert.equal(await check(() => document.querySelector('.ah-streamgraph__crosshair')), null, 'Leaving the plot clears pointer inspection');
    await check(() => document.querySelector('[aria-label="Close project charts"]').focus());
    await key('Tab');
    assert.equal(await check(() => document.activeElement.tagName), 'svg');
    assert.equal(await check(() => document.querySelector('.ah-models svg').dataset.inspectedPoint), '2026-09-01');
    await key('Right');
    assert.equal(await check(() => document.querySelector('.ah-models svg').dataset.inspectedPoint), '2026-09-02');
    await key('End'); await key('Right');
    assert.equal(await check(() => document.querySelector('.ah-models svg').dataset.inspectedPoint), '2026-09-30');
    assert.match(await check(() => document.querySelector('.ah-models svg').getAttribute('aria-valuetext')), /Sep 30, 2026.*tokens.*opus-5-5/);
    await key('Left');
    assert.equal(await check(() => document.querySelector('.ah-models svg').getAttribute('aria-valuenow')), '28');
    await key('Home'); await key('Left');
    assert.equal(await check(() => document.querySelector('.ah-models svg').getAttribute('aria-valuenow')), '0');
    assert.equal(await check(() => document.querySelector('.ah-models .ah-streamgraph__legend').textContent), expanded.legend, 'Inspection preserves each model’s 30-day total');
    await key('Tab');
    assert.equal(await check(() => document.querySelector('.ah-streamgraph__readout')), null, 'Leaving keyboard focus clears inspection');
    await check(() => window.modelsFixture.setName('A project with a very long name that must never displace the tabs'));
    for (const [width, height] of [[1024, 700], [620, 700], [420, 700], [320, 700]]) {
      win.setContentSize(width, height); await pause(200);
      await assertHeaderFits();
      const layout = await check(() => {
        const body = document.querySelector('.ah-models');
        const plot = document.querySelector('.ah-models svg').getBoundingClientRect();
        return { height: body.clientHeight, scrollHeight: body.scrollHeight,
          width: body.clientWidth, scrollWidth: body.scrollWidth, plotHeight: plot.height,
          labels: [...document.querySelectorAll('.ah-models .ah-streamgraph__legend > span')].every(node => {
            const rect = node.getBoundingClientRect(), box = body.getBoundingClientRect();
            return rect.left >= box.left && rect.right <= box.right && rect.bottom <= box.bottom;
          }) };
      });
      assert.ok(layout.scrollHeight <= layout.height + 1 && layout.scrollWidth <= layout.width + 1,
        `Models at ${width}×${height} fit without scrollbars: ${JSON.stringify(layout)}`);
      assert.ok(layout.plotHeight >= 80, 'The plot remains readable');
      if (width >= 420) assert.ok(layout.labels, 'All model labels remain visible');
      else assert.ok(await check(() => {
        const legend = document.querySelector('.ah-models .ah-streamgraph__legend');
        legend.scrollTop = legend.scrollHeight;
        const last = legend.lastElementChild.getBoundingClientRect(), rect = legend.getBoundingClientRect();
        const reachable = last.bottom <= rect.bottom + 1 && last.top >= rect.top;
        legend.scrollTop = 0;
        return reachable;
      }), 'At the smallest width every model remains reachable in the bounded legend');
      await inspect(0); await assertReadoutFits();
      await inspect(29); await assertReadoutFits();
      await shot(`models-hover-${width}.png`);
    }
    await check(() => window.modelsFixture.setName('vibe-field'));
    win.setContentSize(1024, 700); await pause();
    await check(() => document.documentElement.classList.remove('dark'));
    await inspect(24); await assertReadoutFits();
    await shot('models-hover-light.png');
    assert.equal(await check(() => window.modelsFixture.requests.length), 0, 'Pointer and keyboard inspection use already loaded usage');
    await click('[aria-label="View full screen"]');
    assert.ok(await check(() => document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen')));
    await check(() => { window.dialogBeforeSwitch = document.querySelector('[role="dialog"]'); });
    await click('[role="tab"][aria-controls$="-activity"]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), 'Activity did not load on first visit');
    assert.equal(await check(() => window.modelsFixture.requests.length), 1, 'Hourly usage is requested only when Activity is visited');
    assert.ok(await check(() => document.querySelector('[role="dialog"]') === window.dialogBeforeSwitch && window.dialogBeforeSwitch.hasAttribute('data-fullscreen')), 'Switching views keeps the same fullscreen dialog');
    await click('[role="tab"][aria-controls$="-models"]');
    await assertHeaderFits();
    assert.ok(await check(() => document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen')));
    await check(() => document.querySelector('[aria-label="Close project charts"]').focus());
    await key('Tab');
    assert.ok(await check(() => document.activeElement === document.querySelector('.ah-models svg')), 'Tab skips all controls in the hidden Activity panel');
    await key('Escape');
    assert.ok(await check(() => Boolean(document.querySelector('[role="dialog"]') && !document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen'))));
    await key('Escape');
    await until(() => !document.querySelector('[role="dialog"]'), 'Escape did not close the models dialog');
    assert.match(await check(() => document.activeElement.getAttribute('aria-label')), /Expand models chart/);
    await key('Enter');
    await until(() => Boolean(document.querySelector('.ah-models')), 'Keyboard did not reopen the models dialog');
    await pause(300); // Let the previous focus scope finish closing and the new dialog finish its entrance.
    await until(() => Boolean(document.activeElement.closest('[role="dialog"]')), 'Reopening did not move focus into the dialog');
    await key('Tab', ['shift']);
    const focus = await check(() => ({ inside: Boolean(document.activeElement.closest('[role="dialog"]')), nativeFocus: document.hasFocus(),
      active: document.activeElement.outerHTML.slice(0, 700), dialog: Boolean(document.querySelector('[role="dialog"]')) }));
    assert.ok(focus.inside, `Focus stays inside the dialog: ${JSON.stringify(focus)}`);
    await clickAt({ x: 8, y: 8 });
    await until(() => !document.querySelector('[role="dialog"]'), 'Backdrop did not dismiss the models dialog');
    await check(() => window.modelsFixture.setMode('many')); await pause(); await open();
    await until(() => document.querySelectorAll('.ah-models [data-streamgraph-layer]').length === 33, 'A larger project folded its models');
    win.setContentSize(420, 700); await pause(200); await inspect(29); await assertReadoutFits();
    assert.equal(await check(() => document.querySelectorAll('[data-point-series]').length), 33);
    assert.ok(await check(() => [...document.querySelectorAll('[data-point-series^="fixture-model-"] small')]
      .every(node => node.textContent === '<0.1%')), 'Tiny positive model shares are not presented as zero');
    assert.equal(await check(() => Number(document.querySelector('[data-point-total]').dataset.pointTotal)), await check(() => window.modelsFixture.dailyTotals[29]));
    await check(() => document.querySelector('.ah-streamgraph__readout-rows').dispatchEvent(new PointerEvent('pointerover', {
      bubbles: true, pointerType: 'mouse', relatedTarget: document.querySelector('.ah-models svg'),
    }))); await pause();
    assert.equal(await check(() => document.querySelector('.ah-streamgraph__readout')?.dataset.point), '2026-09-30', 'Entering the long hover list preserves its inspected day');
    assert.ok(await check(() => {
      const rows = document.querySelector('.ah-streamgraph__readout-rows'), legend = document.querySelector('.ah-models .ah-streamgraph__legend');
      if (rows.scrollHeight <= rows.clientHeight || legend.scrollHeight <= legend.clientHeight) return false;
      rows.scrollTop = rows.scrollHeight; legend.scrollTop = legend.scrollHeight;
      const last = rows.lastElementChild.getBoundingClientRect(), view = rows.getBoundingClientRect();
      return last.bottom <= view.bottom + 1 && last.top >= view.top;
    }), 'All model rows remain reachable in bounded lists');
    assert.match(await check(() => document.querySelector('.ah-streamgraph__readout-head').textContent), /Sep 30, 2026/, 'The date remains visible while its models scroll');
    await assertReadoutFits(); await shot('models-many-narrow.png');
    const legendScroll = await check(() => {
      window.modelsBeforeSwitch = document.querySelector('.ah-models');
      return document.querySelector('.ah-models .ah-streamgraph__legend').scrollTop;
    });
    await click('[role="tab"][aria-controls$="-activity"]');
    await click('[role="tab"][aria-controls$="-models"]');
    assert.ok(await check(() => document.querySelector('.ah-models') === window.modelsBeforeSwitch), 'Visited Models stays mounted');
    assert.equal(await check(() => document.querySelector('.ah-models .ah-streamgraph__legend').scrollTop), legendScroll, 'The model legend keeps its scroll position');
    await key('Escape');
    await until(() => !document.querySelector('[role="dialog"]'), 'The larger chart did not close');
    win.setContentSize(1024, 700); await pause();
    await check(() => window.modelsFixture.setMode('empty')); await pause(); await open();
    assert.match(await check(() => document.querySelector('.ah-models .ah-streamgraph__empty').textContent), /No model-attributed usage/);
    assert.equal(await check(() => document.querySelectorAll('.ah-models svg').length), 0);
    await key('Escape');
    await check(() => window.modelsFixture.setMode('ready')); await pause(); await open();
    await check(() => window.modelsFixture.switchProject());
    await until(() => !document.querySelector('[role="dialog"]'), 'Project switch kept the previous model dialog');
    await check(() => window.modelsFixture.setMode('unavailable')); await pause();
    assert.ok(await check(() => document.querySelector('.ah-chart-launcher--models button').disabled));
    assert.match(await check(() => document.querySelector('.ah-streamgraph__empty').textContent), /Loading model usage/);
    await check(() => window.modelsFixture.setMode('pending')); await pause();
    assert.ok(await check(() => document.querySelector('.ah-chart-launcher--models button').disabled));
    assert.deepEqual(errors, [], 'Renderer errors');
    console.log(JSON.stringify({ result: 'PASS', screenshots: temporary, checks: ['fullscreen cursor and launch', 'centered tabs with long names at 320–1320px', 'lazy Activity tab and shared fullscreen state', 'model legend scroll survives tab switches', 'individual model bands with matching dates and colors', 'individual model legend totals', 'glow and gradient fill', 'snapped hover guide and markers', 'daily totals and individual model shares', 'zero-activity days', 'keyboard inspection and boundaries', 'one plot without hourly queries', 'compact tooltip in light/dark layouts', '33 models remain accessible without dialog overflow', 'fullscreen, Escape and backdrop', 'keyboard and focus restoration', 'project switch', 'empty and loading states'] }, null, 2));
  } catch (error) {
    failed = true; console.error(error);
    if (win) { await shot('models-failure.png'); console.error({ screenshots: temporary }); }
    if (errors.length) console.error(errors);
  } finally {
    clearInterval(pulse); clearTimeout(deadline); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
void run();
