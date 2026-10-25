const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("octopusSupport", {
  collect: (includeDetails) => ipcRenderer.invoke("octopus:support-collect", { includeDetails }),
  copy: (request) => ipcRenderer.invoke("octopus:support-copy", request),
  save: (request) => ipcRenderer.invoke("octopus:support-save", request),
  openIssue: (request) => ipcRenderer.invoke("octopus:support-issue", request),
});
