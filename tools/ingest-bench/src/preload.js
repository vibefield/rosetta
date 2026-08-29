const { contextBridge, ipcRenderer } = require('electron');

function listener(channel, callback) {
  const wrapped = (_event, payload) => callback(payload);
  ipcRenderer.on(channel, wrapped);
  return () => ipcRenderer.removeListener(channel, wrapped);
}

contextBridge.exposeInMainWorld('rosetta', {
  defaults: () => ipcRenderer.invoke('app:defaults'),
  chooseBinary: () => ipcRenderer.invoke('dialog:binary'),
  chooseDirectory: (title) => ipcRenderer.invoke('dialog:directory', title),
  chooseDatabase: (title) => ipcRenderer.invoke('dialog:database', title),
  chooseJsonl: () => ipcRenderer.invoke('dialog:jsonl'),
  discover: (config) => ipcRenderer.invoke('daemon:discover', config),
  runStage: (config) => ipcRenderer.invoke('benchmark:run-stage', config),
  cancelBenchmark: () => ipcRenderer.invoke('benchmark:cancel'),
  inspectJsonl: (file) => ipcRenderer.invoke('audit:inspect', file),
  auditStreams: (config) => ipcRenderer.invoke('audit:streams', config),
  runJsonlAudit: (config) => ipcRenderer.invoke('audit:run', config),
  cancelJsonlAudit: () => ipcRenderer.invoke('audit:cancel'),
  onAuditEvent: (callback) => listener('audit:event', callback),
  onDaemonEvent: (callback) => listener('daemon:event', callback),
  onBenchmarkEvent: (callback) => listener('benchmark:event', callback),
  openMetaDatabase: (file) => ipcRenderer.invoke('meta-database:open', file),
  metaDatabaseData: () => ipcRenderer.invoke('meta-database:data'),
  openDatabase: (file) => ipcRenderer.invoke('database:open', file),
  databaseOverview: () => ipcRenderer.invoke('database:overview'),
  databaseTable: (name, options) => ipcRenderer.invoke('database:table', name, options),
  databaseQuery: (sql) => ipcRenderer.invoke('database:query', sql),
  reveal: (target) => ipcRenderer.invoke('shell:reveal', target),
});
