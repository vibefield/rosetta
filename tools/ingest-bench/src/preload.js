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
  chooseDatabase: () => ipcRenderer.invoke('dialog:database'),
  discover: (config) => ipcRenderer.invoke('daemon:discover', config),
  runBenchmark: (config) => ipcRenderer.invoke('benchmark:run', config),
  cancelBenchmark: () => ipcRenderer.invoke('benchmark:cancel'),
  onDaemonEvent: (callback) => listener('daemon:event', callback),
  onBenchmarkEvent: (callback) => listener('benchmark:event', callback),
  openDatabase: (file) => ipcRenderer.invoke('database:open', file),
  databaseOverview: () => ipcRenderer.invoke('database:overview'),
  databaseTable: (name, options) => ipcRenderer.invoke('database:table', name, options),
  databaseQuery: (sql) => ipcRenderer.invoke('database:query', sql),
  reveal: (target) => ipcRenderer.invoke('shell:reveal', target),
});
