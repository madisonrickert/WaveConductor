import { app, BrowserWindow } from "electron";
import { spawn, ChildProcess } from "child_process";
import path from "path";
import fs from "fs";
import net from "net";

// Allow audio autoplay without user gesture
app.commandLine.appendSwitch("autoplay-policy", "no-user-gesture-required");

let leapProcess: ChildProcess | null = null;

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
    return;
  }

  const binaryName = getLeapBinaryName();
  if (!binaryName) return;

  const binDir = app.isPackaged
    ? path.join(process.resourcesPath, "bin")
    : path.join(__dirname, "..", "bin");

  const binaryPath = path.join(binDir, binaryName);

  if (!fs.existsSync(binaryPath)) {
    console.warn(`Ultraleap binary not found at ${binaryPath}, skipping`);
    return;
  }

  try {
    leapProcess = spawn(binaryPath, [], {
      stdio: "ignore",
      detached: false,
    });
    leapProcess.on("error", (err) => {
      console.warn("Ultraleap process error:", err.message);
      leapProcess = null;
    });
    console.log(`Ultraleap WebSocket started (pid ${leapProcess.pid})`);
  } catch (err) {
    console.warn("Failed to start Ultraleap:", err);
  }
}

function stopLeapWebSocket() {
  if (leapProcess) {
    leapProcess.kill();
    leapProcess = null;
  }
}

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
}

app.whenReady().then(async () => {
  await startLeapWebSocket();
  createWindow();
});

app.on("window-all-closed", () => {
  app.quit();
});

app.on("before-quit", () => {
  stopLeapWebSocket();
});
