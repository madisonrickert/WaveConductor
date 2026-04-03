import { useState, useEffect, useCallback } from "react";
import { LeapProcessStatus, LeapConnectionStatus } from "@/leap/leapStatus";

export function useLeapStatus() {
    const [processStatus, setProcessStatus] = useState<LeapProcessStatus>("not-started");
    const [connectionStatus, setConnectionStatus] = useState<LeapConnectionStatus>("disconnected");
    const [protocolVersion, setProtocolVersion] = useState<number | null>(null);

    useEffect(() => {
        const api = window.electronAPI;
        if (!api) return;

        api.getLeapProcessStatus().then(setProcessStatus);
        const cleanup = api.onLeapProcessStatus(setProcessStatus);
        return cleanup;
    }, []);

    const startProcess = useCallback(async () => {
        const api = window.electronAPI;
        if (!api) return;
        setProcessStatus(await api.startLeapProcess());
    }, []);

    const stopProcess = useCallback(async () => {
        const api = window.electronAPI;
        if (!api) return;
        setProcessStatus(await api.stopLeapProcess());
    }, []);

    // In the browser (no Electron API), derive process status from connection:
    // if leapjs connected, the server must be running externally.
    const effectiveProcessStatus = window.electronAPI
        ? processStatus
        : connectionStatus !== "disconnected" ? "external" as const : "not-started" as const;

    return { processStatus: effectiveProcessStatus, connectionStatus, setConnectionStatus, protocolVersion, setProtocolVersion, startProcess, stopProcess };
}
