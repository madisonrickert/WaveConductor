import { execFileSync } from "child_process";
import path from "path";

function getBinaryName(): string {
  switch (process.platform) {
    case "win32":
      return "Ultraleap-Tracking-WS-win32.exe";
    case "darwin":
      return process.arch === "x64"
        ? "Ultraleap-Tracking-WS-x86"
        : "Ultraleap-Tracking-WS-arm64";
    default:
      throw new Error(`Unsupported platform: ${process.platform}`);
  }
}

const binaryPath = path.join(__dirname, "..", "bin", getBinaryName());
console.log(`Starting ${path.basename(binaryPath)}...`);
execFileSync(binaryPath, { stdio: "inherit" });
