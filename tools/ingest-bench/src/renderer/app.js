const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];

const state = {
  busy: false,
  discoveries: [],
  selectedAgents: new Set(),
  report: null,
  logs: [],
  logFilter: 'all',
  metaDatabase: null,
  metaData: null,
  selectedMetaProjectId: null,
  expandedMetaSessionIds: new Set(),
  collapsedMetaSessionIds: new Set(),
  metaSessionRenderToken: 0,
  database: null,
  objects: [],
  currentObject: null,
  tableOffset: 0,
  pageSize: 100,
};
const numberFormatter = new Intl.NumberFormat();
const timestampFormatter = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'medium' });

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
  return typeof value === 'number' ? numberFormatter.format(value) : String(value);
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

function updateRunControls() {
  const disabled = state.busy || state.selectedAgents.size === 0;
  $$('.run-stage').forEach((button) => { button.disabled = disabled; });
}

function setBusy(busy, label = 'Ready') {
  state.busy = busy;
  $('#discover-button').disabled = busy;
  updateRunControls();
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
      updateRunControls();
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

function resetPhase(phase) {
  const card = $(`.phase-card[data-phase="${phase}"]`);
  card.classList.remove('running', 'complete', 'failed');
  card.querySelector('.phase-result strong').textContent = '—';
  card.querySelector('.phase-result span').textContent = 'not run';
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

async function runStage(stage) {
  resetPhase(stage);
  $('#result-panel').classList.add('hidden');
  state.report = null;
  setBusy(true, `${stage === 'ingest' ? 'Full ingest' : stage === 'meta' ? 'Meta ingest' : 'Discovery'} running`);
  try {
    const report = await window.rosetta.runStage({
      binary: $('#binary-input').value,
      home: $('#home-input').value,
      outputDir: $('#output-input').value,
      agents: [...state.selectedAgents],
      stage,
    });
    state.report = report;
    renderReport(report);
    if (stage === 'meta') {
      await openMetaDatabase(report.paths.metaDb);
      toast('Meta ingest complete. Its metadata projection is open read-only.');
    } else if (stage === 'ingest') {
      await openDatabase(report.paths.projectionDb);
      toast('Full ingest complete. The published projection is open read-only.');
    } else {
      toast('Discovery benchmark complete.');
    }
  } catch (error) {
    setGlobal(`${stage === 'ingest' ? 'Full ingest' : stage === 'meta' ? 'Meta ingest' : 'Discovery'} failed`, 'failed');
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

function stageLabel(stage) {
  if (stage === 'meta') return 'Meta ingest';
  if (stage === 'ingest') return 'Full ingestion';
  return 'Discovery';
}

function renderReport(report) {
  $('#result-panel').classList.remove('hidden');
  $('#result-title').textContent = report.status === 'complete'
    ? `${stageLabel(report.stage)} complete`
    : `${stageLabel(report.stage)} ${report.status}`;
  const summary = $('#result-summary');
  summary.replaceChildren();
  const artifact = report.stage === 'meta'
    ? report.paths.metaDb
    : report.stage === 'ingest' ? report.paths.projectionDb : report.paths.report;
  const facts = [
    ['agents', report.config.agents.join(', ')],
    ['wall', formatDuration(report.phases[report.stage]?.wallMs)],
    ['artifact', artifact.split(/[\\/]/).pop()],
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
  for (const [name, phase] of Object.entries(report.phases)) {
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

function metaElement(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function formatTimestamp(ms) {
  if (ms == null || !Number.isFinite(Number(ms))) return '—';
  return timestampFormatter.format(new Date(Number(ms)));
}

function formatStructured(value) {
  if (value == null) return '—';
  if (Array.isArray(value)) return value.join(', ') || '—';
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

function searchable(values) {
  return values.flatMap((value) => Array.isArray(value) ? value : [value]).filter((value) => value != null).join(' ').toLowerCase();
}

async function openMetaDatabase(file) {
  if (!file) return;
  try {
    await window.rosetta.openMetaDatabase(file);
    const data = await window.rosetta.metaDatabaseData();
    state.metaDatabase = data.overview;
    state.metaData = data;
    state.selectedMetaProjectId = null;
    state.expandedMetaSessionIds.clear();
    state.collapsedMetaSessionIds.clear();
    $('#meta-project-filter').value = '';
    $('#meta-session-filter').value = '';
    $('#meta-empty').classList.add('hidden');
    $('#meta-content').classList.remove('hidden');
    $('#reveal-meta').disabled = false;
    $('#view-meta-button').disabled = false;
    renderMetaDatabaseOverview(data);
    renderMetaProjects();
    renderMetaSessions();
    switchTab('meta');
  } catch (error) {
    toast(`Could not open meta projection: ${errorMessage(error)}`, 'error');
  }
}

function renderMetaDatabaseOverview(data) {
  const root = $('#meta-summary'); root.replaceChildren();
  const titled = data.sessions.filter((session) => session.title?.trim()).length;
  const unassigned = data.sessions.filter((session) => !session.projectId).length;
  const sources = data.sessions.reduce((sum, session) => sum + Number(session.sourceCount || 0), 0);
  addStat(root, 'agents', formatNumber(data.overview.counts.agent_runtime));
  addStat(root, 'projects', formatNumber(data.projects.length));
  addStat(root, 'sessions', formatNumber(data.sessions.length));
  addStat(root, 'with titles', formatNumber(titled));
  addStat(root, 'unassigned', formatNumber(unassigned));
  addStat(root, 'source files', formatNumber(sources));
}

function renderMetaProjects() {
  if (!state.metaData) return;
  const query = $('#meta-project-filter').value.trim().toLowerCase();
  const projects = state.metaData.projects.filter((project) => searchable([
    project.name, project.id, project.rootPaths, project.gitRepo, project.agentKinds,
  ]).includes(query));
  $('#meta-project-count').textContent = query
    ? `${formatNumber(projects.length)} of ${formatNumber(state.metaData.projects.length)}`
    : formatNumber(projects.length);
  const root = $('#meta-projects'); root.replaceChildren();
  if (!projects.length) {
    root.append(metaElement('div', 'empty-state', 'No project matches this filter.'));
    return;
  }
  for (const project of projects) {
    const active = state.selectedMetaProjectId === project.id;
    const card = metaElement('button', `meta-project-card${active ? ' active' : ''}`);
    card.type = 'button';
    card.setAttribute('aria-pressed', String(active));
    const heading = metaElement('div', 'meta-card-heading');
    heading.append(
      metaElement('strong', '', project.name),
      metaElement('span', 'meta-count', `${formatNumber(project.sessionCount)} session${Number(project.sessionCount) === 1 ? '' : 's'}`),
    );
    card.append(heading);
    card.append(metaElement('code', 'meta-project-id', project.id));
    const facts = metaElement('div', 'meta-project-facts');
    if (project.gitRepo) facts.append(metaElement('span', 'meta-chip', project.gitRepo));
    for (const agent of project.agentKinds) facts.append(metaElement('span', 'meta-chip agent', agent));
    facts.append(metaElement('span', 'meta-muted', `${formatNumber(project.mainSessionCount)} main · ${formatNumber(project.childSessionCount)} child`));
    if (project.latestUpdatedAt) facts.append(metaElement('span', 'meta-muted', `latest ${formatTimestamp(project.latestUpdatedAt)}`));
    card.append(facts);
    const paths = metaElement('div', 'meta-project-paths');
    for (const path of project.rootPaths) paths.append(metaElement('code', '', path));
    if (!project.rootPaths.length) paths.append(metaElement('span', 'meta-muted', 'No root path captured'));
    card.append(paths);
    card.addEventListener('click', () => {
      if (state.selectedMetaProjectId === project.id) return;
      state.selectedMetaProjectId = project.id;
      state.expandedMetaSessionIds.clear();
      state.collapsedMetaSessionIds.clear();
      $('#meta-session-filter').value = '';
      renderMetaProjects();
      renderMetaSessions();
    });
    root.append(card);
  }
}

function appendMetaFact(root, label, value, { mono = false } = {}) {
  if (value == null || value === '') return;
  const item = metaElement('div', 'meta-fact');
  item.append(metaElement('dt', '', label), metaElement('dd', mono ? 'mono' : '', String(value)));
  root.append(item);
}

function sessionCard(session, {
  childCount = 0,
  expanded = false,
  detachedParent = false,
  childrenId = '',
  onToggle = null,
} = {}) {
  const roleClass = session.role === 'main' ? 'main' : 'subagent';
  const card = metaElement('article', `meta-session-card ${roleClass}${detachedParent ? ' detached-parent' : ''}`);
  const heading = metaElement('div', 'meta-card-heading meta-session-heading');
  const titleBlock = metaElement('div', 'meta-session-title-block');
  const title = session.title?.trim();
  titleBlock.append(metaElement('h3', title ? '' : 'missing', title || 'No title captured'));
  titleBlock.append(metaElement('code', 'meta-native-id', session.nativeSessionId));
  const badges = metaElement('div', 'meta-badges');
  badges.append(
    metaElement('span', 'meta-chip agent', session.agentKind),
    metaElement('span', 'meta-chip', session.role),
    metaElement('span', `meta-chip state ${session.streamState}`, session.streamState),
  );
  if (detachedParent) badges.append(metaElement('span', 'meta-chip hierarchy', 'parent outside project'));
  if (childCount > 0) {
    const toggle = metaElement('button', 'meta-session-toggle');
    toggle.type = 'button';
    toggle.setAttribute('aria-expanded', String(expanded));
    toggle.setAttribute('aria-controls', childrenId);
    toggle.title = `${expanded ? 'Collapse' : 'Expand'} ${formatNumber(childCount)} nested subagent${childCount === 1 ? '' : 's'}`;
    const caret = metaElement('span', 'meta-session-toggle-caret', '›');
    caret.setAttribute('aria-hidden', 'true');
    toggle.append(caret, metaElement('span', '', `${formatNumber(childCount)} nested`));
    toggle.addEventListener('click', onToggle);
    badges.append(toggle);
  }
  heading.append(titleBlock, badges);
  card.append(heading);
  if (session.summary?.trim()) {
    const summary = metaElement('p', 'meta-session-summary');
    summary.append(metaElement('span', 'meta-inline-label', 'Summary'), document.createTextNode(session.summary));
    card.append(summary);
  }
  const context = metaElement('div', 'meta-session-context');
  context.append(metaElement('strong', '', session.projectName || 'Unassigned project'));
  if (session.cwd) context.append(metaElement('code', '', session.cwd));
  card.append(context);
  const facts = metaElement('dl', 'meta-fact-grid');
  appendMetaFact(facts, 'Agent', session.agentKind);
  appendMetaFact(facts, 'Runtime enabled', session.runtimeEnabled ? 'yes' : 'no');
  appendMetaFact(facts, 'Project', session.projectName || 'Unassigned');
  appendMetaFact(facts, 'Repository', session.workspaceGitRepo || session.projectGitRepo);
  appendMetaFact(facts, 'Git branch', session.gitBranch);
  appendMetaFact(facts, 'Started', formatTimestamp(session.startedAt));
  appendMetaFact(facts, 'Updated', formatTimestamp(session.updatedAt));
  appendMetaFact(facts, 'Duration', formatDuration(Number(session.updatedAt) - Number(session.startedAt)));
  appendMetaFact(facts, 'Source files', formatNumber(session.sourceCount));
  appendMetaFact(facts, 'Workspace observations', formatNumber(session.workspaceObservationCount));
  appendMetaFact(facts, 'Agent versions', formatStructured(session.versionsSeen));
  appendMetaFact(facts, 'Data root', session.dataRoot, { mono: true });
  appendMetaFact(facts, 'Native session ID', session.nativeSessionId, { mono: true });
  appendMetaFact(facts, 'Canonical session ID', session.id, { mono: true });
  appendMetaFact(facts, 'Runtime ID', session.runtimeId, { mono: true });
  appendMetaFact(facts, 'Project ID', session.projectId, { mono: true });
  appendMetaFact(facts, 'Parent session ID', session.parentSessionId, { mono: true });
  appendMetaFact(facts, 'Root session ID', session.rootSessionId, { mono: true });
  if (session.projectAssignedAt) appendMetaFact(facts, 'Project assigned', formatTimestamp(session.projectAssignedAt));
  card.append(facts);
  return card;
}

function sessionSearchText(session) {
  return searchable([
    session.title, session.summary, session.agentKind, session.role, session.streamState,
    session.projectName, session.cwd, session.nativeSessionId, session.id,
    session.parentSessionId, session.rootSessionId, session.runtimeId, session.dataRoot,
    session.projectRootPaths, session.projectGitRepo, session.workspaceGitRepo, session.gitBranch,
  ]);
}

function metaSessionChildrenId(session) {
  return `meta-session-children-${String(session.id).replace(/[^a-z0-9_-]/gi, '-')}`;
}

function visibleDescendantCount(node) {
  return node.children.reduce((count, child) => count + 1 + visibleDescendantCount(child), 0);
}

function metaSessionTreeNode(node, depth, queryActive) {
  const id = node.session.id;
  const childrenId = metaSessionChildrenId(node.session);
  const autoExpanded = queryActive && node.hasMatchingDescendant;
  const expanded = node.children.length > 0
    && !state.collapsedMetaSessionIds.has(id)
    && (state.expandedMetaSessionIds.has(id) || autoExpanded);
  const wrapper = metaElement('div', `meta-session-node depth-${Math.min(depth, 4)}`);
  const onToggle = () => {
    if (expanded) {
      state.expandedMetaSessionIds.delete(id);
      state.collapsedMetaSessionIds.add(id);
    } else {
      state.expandedMetaSessionIds.add(id);
      state.collapsedMetaSessionIds.delete(id);
    }
    renderMetaSessions();
  };
  wrapper.append(sessionCard(node.session, {
    childCount: visibleDescendantCount(node),
    expanded,
    detachedParent: node.detachedParent,
    childrenId,
    onToggle,
  }));
  if (expanded) {
    const children = metaElement('div', 'meta-session-children');
    children.id = childrenId;
    children.setAttribute('role', 'group');
    children.setAttribute('aria-label', `Subagents of ${node.session.title?.trim() || node.session.nativeSessionId}`);
    for (const child of node.children) children.append(metaSessionTreeNode(child, depth + 1, queryActive));
    wrapper.append(children);
  }
  return wrapper;
}

function renderMetaSessions() {
  if (!state.metaData) return;
  const renderToken = ++state.metaSessionRenderToken;
  const query = $('#meta-session-filter').value.trim().toLowerCase();
  const project = state.metaData.projects.find((item) => item.id === state.selectedMetaProjectId);
  const title = $('#meta-session-title');
  const root = $('#meta-sessions'); root.replaceChildren();
  root.setAttribute('aria-busy', 'false');
  if (!project) {
    title.textContent = 'Choose a project';
    $('#meta-session-count').textContent = '';
    $('#meta-session-scope').textContent = 'Select one project to load its session hierarchy.';
    root.append(metaElement('div', 'empty-state', 'Select a project from the list to view its sessions.'));
    return;
  }
  title.textContent = project.name;
  const sessions = state.metaData.sessions.filter((session) => session.projectId === project.id);
  const matches = query ? (session) => sessionSearchText(session).includes(query) : () => true;
  const forest = window.RosettaMetaTree.buildSessionForest(sessions, matches);
  $('#meta-session-count').textContent = query
    ? `${formatNumber(forest.matchingCount)} of ${formatNumber(forest.total)}`
    : formatNumber(forest.total);
  const detached = forest.detachedCount
    ? ` · ${formatNumber(forest.detachedCount)} parent${forest.detachedCount === 1 ? '' : 's'} outside project`
    : '';
  const scope = query
    ? `${project.name} · ${formatNumber(forest.matchingCount)} matching session${forest.matchingCount === 1 ? '' : 's'} with parent context${detached}.`
    : `${project.name} · ${formatNumber(forest.rootCount)} top-level · ${formatNumber(forest.nestedCount)} nested${detached}. Expand a parent to inspect its subagents.`;
  $('#meta-session-scope').textContent = scope;
  if (!forest.roots.length) {
    root.append(metaElement('div', 'empty-state', 'No session in this project matches the current filter.'));
    return;
  }
  root.setAttribute('aria-busy', 'true');
  const appendChunk = (start) => {
    if (renderToken !== state.metaSessionRenderToken) return;
    const fragment = document.createDocumentFragment();
    const end = Math.min(start + 40, forest.roots.length);
    for (let index = start; index < end; index += 1) {
      fragment.append(metaSessionTreeNode(forest.roots[index], 0, Boolean(query)));
    }
    root.append(fragment);
    if (end < forest.roots.length) {
      $('#meta-session-scope').textContent = `${scope} Rendering ${formatNumber(end)} of ${formatNumber(forest.roots.length)} top-level groups…`;
      requestAnimationFrame(() => appendChunk(end));
    } else {
      root.setAttribute('aria-busy', 'false');
      $('#meta-session-scope').textContent = scope;
    }
  };
  appendChunk(0);
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
  $$('.run-stage').forEach((button) => button.addEventListener('click', () => runStage(button.dataset.stage)));
  $('#stop-button').addEventListener('click', async () => { await window.rosetta.cancelBenchmark(); setGlobal('Stopping daemon', 'running'); });
  $('#select-all').addEventListener('click', () => { state.selectedAgents = new Set(state.discoveries.filter((item) => item.installed).map((item) => item.agent)); renderAgents(); updateRunControls(); });
  $('#select-none').addEventListener('click', () => { state.selectedAgents.clear(); renderAgents(); updateRunControls(); });
  $('#reveal-run').addEventListener('click', () => state.report && window.rosetta.reveal(state.report.paths.report));
  $('#view-meta-button').addEventListener('click', () => switchTab('meta'));
  $('#open-meta-button').addEventListener('click', async () => { const file = await window.rosetta.chooseDatabase('Open a meta.sqlite projection'); if (file) openMetaDatabase(file); });
  $('#reveal-meta').addEventListener('click', () => state.metaDatabase && window.rosetta.reveal(state.metaDatabase.file));
  $('#meta-project-filter').addEventListener('input', renderMetaProjects);
  $('#meta-session-filter').addEventListener('input', () => {
    state.collapsedMetaSessionIds.clear();
    renderMetaSessions();
  });
  $('#open-db-button').addEventListener('click', async () => { const file = await window.rosetta.chooseDatabase('Open a Rosetta SQLite projection'); if (file) openDatabase(file); });
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
