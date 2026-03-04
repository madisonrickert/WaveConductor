import { LeapProcessStatus } from "@/common/leapStatus";

interface ElectronAPI {
  getLeapProcessStatus: () => Promise<LeapProcessStatus>;
  startLeapProcess: () => Promise<LeapProcessStatus>;
  stopLeapProcess: () => Promise<LeapProcessStatus>;
  onLeapProcessStatus: (callback: (status: LeapProcessStatus) => void) => () => void;
  startPowerSaveBlocker: () => Promise<void>;
  stopPowerSaveBlocker: () => Promise<void>;
}

declare global {
  interface Window {
    electronAPI?: ElectronAPI;
  }
}

export {};
