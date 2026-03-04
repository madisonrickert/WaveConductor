import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("electronAPI", {
  getLeapProcessStatus: () => ipcRenderer.invoke("get-leap-process-status"),
  startLeapProcess: () => ipcRenderer.invoke("start-leap-process"),
  stopLeapProcess: () => ipcRenderer.invoke("stop-leap-process"),
  onLeapProcessStatus: (callback: (status: string) => void) => {
    const handler = (_event: Electron.IpcRendererEvent, status: string) => callback(status);
    ipcRenderer.on("leap-process-status", handler);
    return () => {
      ipcRenderer.removeListener("leap-process-status", handler);
    };
  },
  startPowerSaveBlocker: () => ipcRenderer.invoke("start-power-save-blocker"),
  stopPowerSaveBlocker: () => ipcRenderer.invoke("stop-power-save-blocker"),
});
