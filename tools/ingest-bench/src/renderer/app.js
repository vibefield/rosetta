const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];

const state = {
  busy: false,
  discoveries: [],
  selectedAgents: new Set(),
  report: null,
  logs: [],
  logFilter: 'all',
  database: null,
  objects: [],
  currentObject: null,
  tableOffset: 0,
  pageSize: 100,
};

function formatDuration(ms) {
  if (ms == null || !Number.isFinite(Number(ms))) return '—';
  const value = Number(ms);
  if (value < 1_000) return `${value.toFixed(value < 10 ? 1 : 0)} ms`;
  if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`;
  const minutes = Math.floor(value / 60_000);
  return `${minutes}m ${((value % 60_000) / 1_000).toFixed(1)}s`;
}

function formatBytes(bytes) {
  if (bytes == null || !Number.isFinite(Number(bytes))) return '—';
  let value = Number(bytes);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value.toFixed(unit === 0 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

function formatNumber(value) {
  if (value == null) return '—';
  return typeof value === 'number' ? new Intl.NumberFormat().format(value) : String(value);
}

function formatCell(value) {
  if (value === null) return 'NULL';
  if (value === undefined) return '';
  if (typeof value === 'object') {
    if (value.$type === 'text') return `${value.text}… [${formatNumber(value.chars)} chars]`;
    if (value.$type === 'blob') return `<BLOB ${formatBytes(value.bytes)} · ${value.hex}${value.truncated ? '…' : ''}>`;
    return JSON.stringify(value);
  }
  return String(value);
}

function errorMessage(error) {
  const raw = error?.message || String(error);
  const start = raw.indexOf('{');
  if (start >= 0) {
    try { return JSON.parse(raw.slice(start)).message || raw; } catch { /* use raw */ }
  }
  return raw.replace(/^Error invoking remote method '[^']+': Error: /, '');
}

let toastTimer;
function toast(message, kind = '') {
  const element = $('#toast');
  element.textContent = message;
  element.className = `toast show ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { element.className = 'toast'; }, 4_000);
}

function setGlobal(label, mode = '') {
  const global = $('#global-state');
  global.className = `global-state ${mode}`;
  global.lastElementChild.textContent = label;
}

function setBusy(busy, label = 'Ready') {
  state.busy = busy;
  $('#discover-button').disabled = busy;
  $('#run-button').disabled = busy || state.selectedAgents.size === 0;
  $('#stop-button').disabled = !busy;
  $('#binary-input').disabled = busy;
  $('#home-input').disabled = busy;
  $('#output-input').disabled = busy;
  setGlobal(label, busy ? 'running' : '');
}

function switchTab(name) {
  $$('.tab').forEach((tab) => tab.classList.toggle('active', tab.dataset.tab === name));
  $$('.view').forEach((view) => view.classList.toggle('active', view.id === `${name}-view`));
}

function appendLog(entry) {
  state.logs.push({ at: new Date().toISOString().slice(11, 23), ...entry });
  if (state.logs.length > 5_000) state.logs.shift();
  $('#log-count').textContent = formatNumber(state.logs.length);
  renderLogs();
}

function renderLogs() {
  const visible = state.logs.filter((entry) => state.logFilter === 'all' || entry.stream === state.logFilter);
  const log = $('#logs');
  log.textContent = visible.length
    ? visible.map((entry) => `${entry.at}  ${entry.phase.padEnd(10)} ${entry.stream.padEnd(6)}  ${entry.line}`).join('\n')
    : 'No daemon output yet.';
  if ($('#logs-view').classList.contains('active')) log.scrollTop = log.scrollHeight;
}

function renderAgents() {
  const root = $('#agents');
  root.replaceChildren();
  root.className = 'agents';
  if (state.discoveries.length === 0) {
    root.className = 'agents empty-state';
    root.textContent = 'No registered runtime returned by discovery.';
    return;
  }
  for (const discovery of state.discoveries) {
    const label = document.createElement('label');
    label.className = `agent ${discovery.installed ? '' : 'uninstalled'}`;
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.disabled = !discovery.installed || state.busy;
    input.checked = state.selectedAgents.has(discovery.agent);
    input.addEventListener('change', () => {
      if (input.checked) state.selectedAgents.add(discovery.agent);
      else state.selectedAgents.delete(discovery.agent);
      $('#run-button').disabled = state.busy || state.selectedAgents.size === 0;
    });
    const name = document.createElement('strong');
    name.textContent = discovery.agent;
    const facts = document.createElement('small');
    facts.textContent = discovery.installed
      ? `${formatNumber(discovery.files)} files · ${formatBytes(discovery.bytes)}`
      : 'not installed';
    facts.title = discovery.resolvedRoot || discovery.dataRoot;
    label.append(input, name, facts);
    root.append(label);
  }
}

function resetPhases() {
  $$('.phase-card').forEach((card) => {
    card.classList.remove('running', 'complete', 'failed');
    card.querySelector('.phase-result strong').textContent = '—';
    card.querySelector('.phase-result span').textContent = 'not run';
  });
}

function updatePhase(phase, mode, report = null) {
  const card = $(`.phase-card[data-phase="${phase}"]`);
  if (!card) return;
  card.classList.remove('running', 'complete', 'failed');
  if (mode) card.classList.add(mode);
  const strong = card.querySelector('.phase-result strong');
  const detail = card.querySelector('.phase-result span');
  if (mode === 'running') { strong.textContent = '…'; detail.textContent = 'daemon active'; }
  if (mode === 'complete') { strong.textContent = formatDuration(report?.wallMs); detail.textContent = 'external wall time'; }
  if (mode === 'failed') { strong.textContent = report?.wallMs ? formatDuration(report.wallMs) : 'failed'; detail.textContent = report?.error?.message || 'see daemon log'; }
}

async function runDiscovery() {
  setBusy(true, 'Discovering');
  appendLog({ phase: 'preview', stream: 'stdout', line: 'Starting discovery preview' });
  try {
    const result = await window.rosetta.discover({
      binary: $('#binary-input').value,
      home: $('#home-input').value,
    });
    state.discoveries = result.discoveries;
    state.selectedAgents = new Set(result.discoveries.filter((item) => item.installed).map((item) => item.agent));
    renderAgents();
    toast(`Found ${state.selectedAgents.size} installed runtime${state.selectedAgents.size === 1 ? '' : 's'}.`);
  } catch (error) {
    setGlobal('Discovery failed', 'failed');
    toast(errorMessage(error), 'error');
  } finally {
    setBusy(false);
    renderAgents();
  }
}

async function runBenchmark() {
  resetPhases();
  $('#result-panel').classList.add('hidden');
  state.report = null;
  setBusy(true, 'Benchmark running');
  try {
    const report = await window.rosetta.runBenchmark({
      binary: $('#binary-input').value,
      home: $('#home-input').value,
      outputDir: $('#output-input').value,
      agents: [...state.selectedAgents],
    });
    state.report = report;
    renderReport(report);
    await openDatabase(report.paths.projectionDb);
    toast('Cold ingest complete. The published projection is open read-only.');
  } catch (error) {
    setGlobal('Benchmark failed', 'failed');
    toast(errorMessage(error), 'error');
  } finally {
    setBusy(false);
  }
}

function metric(phase, name) {
  const value = phase?.profile?.[name];
  if (!value) return '—';
  return value.unit === 'ms' ? formatDuration(value.value) : `${formatNumber(value.value)} ${value.unit}`;
}

function renderReport(report) {
  $('#result-panel').classList.remove('hidden');
  $('#result-title').textContent = report.status === 'complete' ? 'Cold run published' : `Run ${report.status}`;
  const summary = $('#result-summary');
  summary.replaceChildren();
  const facts = [
    ['agents', report.config.agents.join(', ')],
    ['total wall', formatDuration(Object.values(report.phases).reduce((sum, phase) => sum + (phase.wallMs || 0), 0))],
    ['projection', report.paths.projectionDb.split(/[\\/]/).pop()],
    ['report', report.paths.report.split(/[\\/]/).pop()],
  ];
  for (const [label, value] of facts) {
    const item = document.createElement('div');
    item.className = 'summary-item';
    const key = document.createElement('span'); key.textContent = label;
    const val = document.createElement('strong'); val.textContent = value; val.title = value;
    item.append(key, val); summary.append(item);
  }
  const table = $('#phase-table');
  table.replaceChildren();
  for (const name of ['discovery', 'meta', 'ingest']) {
    const phase = report.phases[name] || {};
    const row = document.createElement('tr');
    for (const value of [name, formatDuration(phase.wallMs), metric(phase, 'walk time'), metric(phase, 'skim time'), metric(phase, 'read time'), metric(phase, 'normalize time'), metric(phase, 'apply time'), metric(phase, 'bulk FTS time')]) {
      const cell = document.createElement('td'); cell.textContent = value; cell.className = 'mono'; row.append(cell);
    }
    table.append(row);
  }
}

function benchmarkEvent(message) {
  if (message.type === 'log') appendLog(message);
  if (message.type === 'phase-start') updatePhase(message.phase, 'running');
  if (message.type === 'phase-complete') updatePhase(message.phase, 'complete', message.phaseReport);
  if (message.type === 'phase-failed') updatePhase(message.phase, 'failed', message.phaseReport);
  if (message.type === 'run-finish') {
    state.report = message.report;
    renderReport(message.report);
  }
}

function addStat(root, label, value) {
  const stat = document.createElement('div'); stat.className = 'stat';
  const key = document.createElement('span'); key.textContent = label;
  const val = document.createElement('strong'); val.textContent = value; val.title = value;
  stat.append(key, val); root.append(stat);
}

async function openDatabase(file) {
  if (!file) return;
  try {
    await window.rosetta.openDatabase(file);
    const overview = await window.rosetta.databaseOverview();
    state.database = overview;
    state.objects = overview.objects.filter((object) => object.type === 'table' || object.type === 'view');
    $('#db-empty').classList.add('hidden');
    $('#db-content').classList.remove('hidden');
    $('#reveal-db').disabled = false;
    renderDatabaseOverview(overview);
    renderObjects();
    switchTab('database');
  } catch (error) {
    toast(`Could not open projection: ${errorMessage(error)}`, 'error');
  }
}

function renderDatabaseOverview(overview) {
  const root = $('#db-summary'); root.replaceChildren();
  addStat(root, 'schema', `v${overview.schemaVersion}`);
  addStat(root, 'file size', formatBytes(overview.bytes));
  addStat(root, 'runtimes', formatNumber(overview.counts.agent_runtime));
  addStat(root, 'projects', formatNumber(overview.counts.project));
  addStat(root, 'sessions', formatNumber(overview.counts.session));
  addStat(root, 'messages', formatNumber(overview.counts.message));
}

function renderObjects() {
  const filter = $('#object-filter').value.toLowerCase();
  const root = $('#objects'); root.replaceChildren();
  for (const object of state.objects.filter((item) => item.name.toLowerCase().includes(filter))) {
    const button = document.createElement('button');
    button.className = `object ${state.currentObject === object.name ? 'active' : ''}`;
    const name = document.createElement('span'); name.textContent = object.name;
    const type = document.createElement('span'); type.textContent = object.type;
    button.append(name, type);
    button.addEventListener('click', () => loadTable(object.name, 0));
    root.append(button);
  }
}

async function loadTable(name, offset = 0) {
  try {
    const result = await window.rosetta.databaseTable(name, { limit: state.pageSize, offset });
    state.currentObject = name;
    state.tableOffset = result.offset;
    $('#object-kind').textContent = result.type;
    $('#object-title').textContent = result.name;
    $('#object-ddl').textContent = result.sql || 'No DDL text available.';
    $('#schema-details').open = false;
    const from = result.total === 0 ? 0 : result.offset + 1;
    const to = Math.min(Number(result.total), result.offset + result.rows.length);
    $('#page-label').textContent = `${formatNumber(from)}–${formatNumber(to)} / ${formatNumber(result.total)}`;
    $('#prev-page').disabled = result.offset === 0;
    $('#next-page').disabled = result.offset + result.rows.length >= Number(result.total);
    renderDataTable($('#data-table'), result);
    renderObjects();
  } catch (error) {
    toast(errorMessage(error), 'error');
  }
}

function showCell(column, value) {
  $('#cell-title').textContent = column;
  $('#cell-value').textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
  $('#cell-dialog').showModal();
}

function renderDataTable(root, result) {
  root.replaceChildren();
  if (!result.rows?.length) {
    const empty = document.createElement('div'); empty.className = 'empty-state'; empty.textContent = 'Query returned no rows.'; root.append(empty); return;
  }
  const table = document.createElement('table');
  const head = document.createElement('thead'); const headRow = document.createElement('tr');
  for (const column of result.columns) { const th = document.createElement('th'); th.textContent = column; headRow.append(th); }
  head.append(headRow); table.append(head);
  const body = document.createElement('tbody');
  for (const row of result.rows) {
    const tr = document.createElement('tr');
    for (const column of result.columns) {
      const td = document.createElement('td');
      td.textContent = formatCell(row[column]);
      td.title = 'Click to inspect';
      td.addEventListener('click', () => showCell(column, row[column]));
      tr.append(td);
    }
    body.append(tr);
  }
  table.append(body); root.append(table);
}

async function runQuery() {
  const started = performance.now();
  $('#run-query').disabled = true;
  $('#query-meta').textContent = 'Running…';
  try {
    const result = await window.rosetta.databaseQuery($('#query-input').value);
    renderDataTable($('#query-result'), result);
    $('#query-meta').textContent = `${formatNumber(result.rows.length)} rows · ${formatDuration(performance.now() - started)}${result.limitedTo ? ` · capped at ${formatNumber(result.limitedTo)}` : ''}`;
  } catch (error) {
    $('#query-meta').textContent = errorMessage(error);
    toast(errorMessage(error), 'error');
  } finally {
    $('#run-query').disabled = false;
  }
}

async function init() {
  const defaults = await window.rosetta.defaults();
  $('#binary-input').value = defaults.binary;
  $('#home-input').value = defaults.home;
  $('#output-input').value = defaults.outputDir;
  setBusy(false);

  $$('.tab').forEach((tab) => tab.addEventListener('click', () => switchTab(tab.dataset.tab)));
  $('#binary-browse').addEventListener('click', async () => { const file = await window.rosetta.chooseBinary(); if (file) $('#binary-input').value = file; });
  $('#home-browse').addEventListener('click', async () => { const dir = await window.rosetta.chooseDirectory('Choose the home directory to probe'); if (dir) $('#home-input').value = dir; });
  $('#output-browse').addEventListener('click', async () => { const dir = await window.rosetta.chooseDirectory('Choose benchmark output directory'); if (dir) $('#output-input').value = dir; });
  $('#discover-button').addEventListener('click', runDiscovery);
  $('#run-button').addEventListener('click', runBenchmark);
  $('#stop-button').addEventListener('click', async () => { await window.rosetta.cancelBenchmark(); setGlobal('Stopping daemon', 'running'); });
  $('#select-all').addEventListener('click', () => { state.selectedAgents = new Set(state.discoveries.filter((item) => item.installed).map((item) => item.agent)); renderAgents(); $('#run-button').disabled = state.busy || state.selectedAgents.size === 0; });
  $('#select-none').addEventListener('click', () => { state.selectedAgents.clear(); renderAgents(); $('#run-button').disabled = true; });
  $('#reveal-run').addEventListener('click', () => state.report && window.rosetta.reveal(state.report.paths.report));
  $('#open-db-button').addEventListener('click', async () => { const file = await window.rosetta.chooseDatabase(); if (file) openDatabase(file); });
  $('#reveal-db').addEventListener('click', () => state.database && window.rosetta.reveal(state.database.file));
  $('#object-filter').addEventListener('input', renderObjects);
  $('#prev-page').addEventListener('click', () => state.currentObject && loadTable(state.currentObject, Math.max(0, state.tableOffset - state.pageSize)));
  $('#next-page').addEventListener('click', () => state.currentObject && loadTable(state.currentObject, state.tableOffset + state.pageSize));
  $('#run-query').addEventListener('click', runQuery);
  $('#query-input').addEventListener('keydown', (event) => { if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') runQuery(); });
  $('#clear-logs').addEventListener('click', () => { state.logs = []; $('#log-count').textContent = '0'; renderLogs(); });
  $$('.log-filter').forEach((button) => button.addEventListener('click', () => { state.logFilter = button.dataset.stream; $$('.log-filter').forEach((item) => item.classList.toggle('active', item === button)); renderLogs(); }));
  $('#close-dialog').addEventListener('click', () => $('#cell-dialog').close());
  window.rosetta.onDaemonEvent((message) => { if (message.type === 'log') appendLog(message); });
  window.rosetta.onBenchmarkEvent(benchmarkEvent);
}

init().catch((error) => toast(errorMessage(error), 'error'));
