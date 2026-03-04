import { app, BrowserWindow, ipcMain, powerSaveBlocker } from "electron";
import { spawn, ChildProcess } from "child_process";
import path from "path";
import fs from "fs";
import net from "net";

// Allow audio autoplay without user gesture
app.commandLine.appendSwitch("autoplay-policy", "no-user-gesture-required");

type LeapProcessStatus = "not-started" | "running" | "errored" | "exited" | "external";

let leapProcess: ChildProcess | null = null;
let leapProcessStatus: LeapProcessStatus = "not-started";
let mainWindow: BrowserWindow | null = null;
let externalPollTimer: NodeJS.Timeout | null = null;

function setLeapProcessStatus(status: LeapProcessStatus) {
  leapProcessStatus = status;
  mainWindow?.webContents.send("leap-process-status", status);

  // Poll the external server so we notice if it goes away
  if (status === "external") {
    startExternalPoll();
  } else {
    stopExternalPoll();
  }
}

const EXTERNAL_POLL_INTERVAL_MS = 3000;

function startExternalPoll() {
  if (externalPollTimer) return;
  externalPollTimer = setInterval(async () => {
    if (leapProcessStatus !== "external") {
      stopExternalPoll();
      return;
    }
    if (!(await isPortListening(6437))) {
      console.log("External Leap WebSocket server is no longer reachable");
      setLeapProcessStatus("exited");
    }
  }, EXTERNAL_POLL_INTERVAL_MS);
}

function stopExternalPoll() {
  if (externalPollTimer) {
    clearInterval(externalPollTimer);
    externalPollTimer = null;
  }
}

function getLeapBinaryName(): string | null {
  switch (process.platform) {
    case "darwin":
      return "Ultraleap-Tracking-WS-macos-applesilicon";
    case "win32":
      return "Ultraleap-Tracking-WS-win32.exe";
    default:
      return null;
  }
}

function isPortListening(port: number, timeoutMs = 1000): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = new net.Socket();
    socket.setTimeout(timeoutMs);
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("timeout", () => {
      socket.destroy();
      resolve(false);
    });
    socket.once("error", () => {
      socket.destroy();
      resolve(false);
    });
    socket.connect(port, "127.0.0.1");
  });
}

async function startLeapWebSocket() {
  if (await isPortListening(6437)) {
    console.log(
      "Leap WebSocket server already running on port 6437, skipping binary"
    );
    setLeapProcessStatus("external");
    return;
  }

  const binaryName = getLeapBinaryName();
  if (!binaryName) {
    setLeapProcessStatus("not-started");
    return;
  }

  const binDir = app.isPackaged
    ? path.join(process.resourcesPath, "bin")
    : path.join(__dirname, "..", "bin");

  const binaryPath = path.join(binDir, binaryName);

  if (!fs.existsSync(binaryPath)) {
    console.warn(`Ultraleap binary not found at ${binaryPath}, skipping`);
    setLeapProcessStatus("not-started");
    return;
  }

  try {
    leapProcess = spawn(binaryPath, [], {
      stdio: "ignore",
      detached: false,
    });
    leapProcess.on("error", (err) => {
      console.warn("Ultraleap process error:", err.message);
      if (leapProcess) {
        leapProcess = null;
        setLeapProcessStatus("errored");
      }
    });
    leapProcess.on("exit", (code) => {
      console.log(`Ultraleap process exited with code ${code}`);
      if (leapProcess) {
        leapProcess = null;
        setLeapProcessStatus("exited");
      }
    });
    console.log(`Ultraleap WebSocket started (pid ${leapProcess.pid})`);
    setLeapProcessStatus("running");
  } catch (err) {
    console.warn("Failed to start Ultraleap:", err);
    setLeapProcessStatus("errored");
  }
}

function stopLeapWebSocket() {
  if (leapProcess) {
    leapProcess.kill();
    leapProcess = null;
  }
}

// --- Power Save Blocker ---
let powerSaveBlockerId: number | null = null;

ipcMain.handle("start-power-save-blocker", () => {
  if (powerSaveBlockerId !== null && powerSaveBlocker.isStarted(powerSaveBlockerId)) {
    return;
  }
  powerSaveBlockerId = powerSaveBlocker.start("prevent-display-sleep");
});

ipcMain.handle("stop-power-save-blocker", () => {
  if (powerSaveBlockerId !== null && powerSaveBlocker.isStarted(powerSaveBlockerId)) {
    powerSaveBlocker.stop(powerSaveBlockerId);
    powerSaveBlockerId = null;
  }
});

// IPC handlers
ipcMain.handle("get-leap-process-status", () => {
  return leapProcessStatus;
});

ipcMain.handle("start-leap-process", async () => {
  if (leapProcess) return leapProcessStatus;
  await startLeapWebSocket();
  return leapProcessStatus;
});

ipcMain.handle("stop-leap-process", () => {
  stopLeapWebSocket();
  // stopLeapWebSocket sets leapProcess = null before exit handler fires,
  // so the exit handler guard won't trigger. Set status explicitly.
  if (leapProcessStatus !== "exited") {
    setLeapProcessStatus("exited");
  }
  return leapProcessStatus;
});

function createWindow() {
  const win = new BrowserWindow({
    width: 1920,
    height: 1080,
    fullscreen: true,
    backgroundColor: "#000000",
    autoHideMenuBar: true,
    show: false,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  win.once("ready-to-show", () => {
    win.show();
  });

  if (process.env.VITE_DEV_SERVER_URL) {
    win.loadURL(process.env.VITE_DEV_SERVER_URL);
  } else {
    win.loadFile(path.join(__dirname, "..", "dist", "index.html"));
  }

  mainWindow = win;
  win.on("closed", () => {
    mainWindow = null;
  });
}

app.whenReady().then(async () => {
  await startLeapWebSocket();
  createWindow();
});

app.on("window-all-closed", () => {
  app.quit();
});

app.on("before-quit", () => {
  stopExternalPoll();
  stopLeapWebSocket();
});
