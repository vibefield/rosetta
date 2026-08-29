const { app, BrowserWindow, dialog, ipcMain, shell } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const { BenchmarkRun } = require('./lib/benchmark');
const { ProjectionDatabase } = require('./lib/database');
const { AGENTS, assertDirectory, assertFile, discover } = require('./lib/daemon');
const {
  JsonlAuditRun,
  inspectJsonl,
  sourceInventory,
  streamSpecs,
} = require('./lib/jsonl-audit');

const REPO_ROOT = path.resolve(__dirname, '../../..');
const projectionDatabase = new ProjectionDatabase();
const metaDatabase = new ProjectionDatabase();
let window = null;
let activeRun = null;
let activeDiscovery = null;
let activeAudit = null;

function send(channel, payload) {
  if (window && !window.isDestroyed()) window.webContents.send(channel, payload);
}

function targetDirectory() {
  const metadata = spawnSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    timeout: 10_000,
  });
  if (metadata.status !== 0) return path.join(REPO_ROOT, 'target');
  try {
    return JSON.parse(metadata.stdout).target_directory;
  } catch {
    return path.join(REPO_ROOT, 'target');
  }
}

function defaultBinary() {
  const target = targetDirectory();
  const executable = process.platform === 'win32' ? 'rosettad.exe' : 'rosettad';
  const candidates = [path.join(target, 'release', executable), path.join(target, 'debug', executable)];
  return candidates.find((candidate) => fs.existsSync(candidate)) || candidates[0];
}

function serializeFailure(error) {
  return {
    name: error?.name || 'Error',
    message: error?.message || String(error),
    details: error?.details || null,
    report: error?.benchmarkReport || null,
  };
}

function createWindow() {
  window = new BrowserWindow({
    width: 1440,
    height: 940,
    minWidth: 1040,
    minHeight: 720,
    title: 'Rosetta Ingest Bench',
    backgroundColor: '#FAF9F7',
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  window.loadFile(path.join(__dirname, 'renderer', 'index.html'));
  window.on('closed', () => {
    activeRun?.cancel();
    window = null;
  });
}

ipcMain.handle('app:defaults', () => ({
  repoRoot: REPO_ROOT,
  binary: defaultBinary(),
  home: os.homedir(),
  outputDir: path.join(app.getPath('documents'), 'Rosetta Ingest Bench'),
  agents: [...AGENTS],
}));

ipcMain.handle('dialog:jsonl', async () => {
  const result = await dialog.showOpenDialog(window, {
    title: 'Choose JSONL files to audit',
    properties: ['openFile', 'multiSelections'],
    filters: [{ name: 'JSON Lines', extensions: ['jsonl', 'ndjson'] }, { name: 'All files', extensions: ['*'] }],
  });
  return result.canceled ? [] : result.filePaths;
});

ipcMain.handle('dialog:binary', async () => {
  const result = await dialog.showOpenDialog(window, {
    title: 'Choose the rosettad binary',
    properties: ['openFile'],
  });
  return result.canceled ? null : result.filePaths[0];
});

ipcMain.handle('dialog:directory', async (_event, title) => {
  const result = await dialog.showOpenDialog(window, {
    title: typeof title === 'string' ? title : 'Choose a directory',
    properties: ['openDirectory', 'createDirectory'],
  });
  return result.canceled ? null : result.filePaths[0];
});

ipcMain.handle('dialog:database', async (_event, title) => {
  const result = await dialog.showOpenDialog(window, {
    title: typeof title === 'string' ? title : 'Open a Rosetta SQLite projection',
    properties: ['openFile'],
    filters: [{ name: 'SQLite', extensions: ['sqlite', 'sqlite3', 'db'] }, { name: 'All files', extensions: ['*'] }],
  });
  return result.canceled ? null : result.filePaths[0];
});

ipcMain.handle('daemon:discover', async (_event, config) => {
  if (activeRun || activeDiscovery) throw new Error('another daemon process is already running');
  const binary = assertFile(config.binary, 'rosettad binary');
  const home = assertDirectory(config.home, 'home directory');
  const events = [];
  try {
    const resultPromise = discover(binary, home, {
      onEvent: (event) => {
        events.push(event);
        send('daemon:event', { type: 'daemon-event', phase: 'preview', event });
      },
      onLog: (entry) => send('daemon:event', { type: 'log', phase: 'preview', ...entry }),
    });
    // Preview discovery is finite; it intentionally cannot overlap a benchmark.
    activeDiscovery = resultPromise;
    return await resultPromise;
  } catch (error) {
    throw new Error(JSON.stringify(serializeFailure(error)));
  } finally {
    activeDiscovery = null;
  }
});

ipcMain.handle('benchmark:run-stage', async (_event, config) => {
  if (activeRun || activeDiscovery) throw new Error('another daemon process is already running');
  try {
    activeRun = new BenchmarkRun(config, (event) => send('benchmark:event', event));
    return await activeRun.execute();
  } catch (error) {
    throw new Error(JSON.stringify(serializeFailure(error)));
  } finally {
    activeRun = null;
  }
});

ipcMain.handle('benchmark:cancel', () => {
  activeRun?.cancel();
  return Boolean(activeRun);
});

ipcMain.handle('audit:inspect', (_event, file) => inspectJsonl(file));

ipcMain.handle('audit:streams', async (_event, config) => {
  if (activeRun || activeDiscovery || activeAudit) throw new Error('another daemon process is already running');
  return streamSpecs(assertFile(config.binary, 'rosettad binary'), config.agent);
});

ipcMain.handle('audit:sources', async (_event, config) => {
  if (activeRun || activeDiscovery || activeAudit) throw new Error('another daemon process is already running');
  return sourceInventory(config.binary, config.home, config.agent, config.limit);
});

ipcMain.handle('audit:run', async (_event, config) => {
  if (activeRun || activeDiscovery || activeAudit) throw new Error('another daemon process is already running');
  try {
    activeAudit = new JsonlAuditRun(config, (event) => send('audit:event', event));
    return await activeAudit.execute();
  } catch (error) {
    throw new Error(JSON.stringify(serializeFailure(error)));
  } finally {
    activeAudit = null;
  }
});

ipcMain.handle('audit:cancel', () => {
  activeAudit?.cancel();
  return Boolean(activeAudit);
});

ipcMain.handle('meta-database:open', (_event, file) => metaDatabase.open(file));
ipcMain.handle('meta-database:data', () => metaDatabase.metaData());

ipcMain.handle('database:open', (_event, file) => projectionDatabase.open(file));
ipcMain.handle('database:overview', () => projectionDatabase.overview());
ipcMain.handle('database:table', (_event, name, options) => projectionDatabase.table(name, options));
ipcMain.handle('database:query', (_event, sql) => projectionDatabase.query(sql));

ipcMain.handle('shell:reveal', async (_event, target) => {
  const resolved = path.resolve(target);
  shell.showItemInFolder(resolved);
  return resolved;
});

app.whenReady().then(() => {
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('before-quit', () => {
  activeRun?.cancel();
  activeAudit?.cancel();
  metaDatabase.close();
  projectionDatabase.close();
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
