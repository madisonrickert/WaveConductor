import React, { useEffect, useRef, useState } from "react";
import { LeapProcessStatus, LeapConnectionStatus } from "@/common/leapStatus";

import "./leapStatusIndicator.scss";

interface LeapStatusIndicatorProps {
    processStatus: LeapProcessStatus;
    connectionStatus: LeapConnectionStatus;
    protocolVersion: number | null;
    onStart: () => void;
    onStop: () => void;
}

const CONNECTION_LABELS: Record<LeapConnectionStatus, string> = {
    disconnected: "Disconnected",
    connected: "Server Only",
    streaming: "Streaming",
};

const CONNECTION_COLORS: Record<LeapConnectionStatus, string> = {
    disconnected: "#e74c3c",
    connected: "#f39c12",
    streaming: "#2ecc71",
};

const PROCESS_LABELS: Record<LeapProcessStatus, string> = {
    "not-started": "Not Started",
    running: "Running",
    errored: "Errored",
    exited: "Exited",
    external: "External",
};

const PROCESS_COLORS: Record<LeapProcessStatus, string> = {
    "not-started": "#888",
    running: "#2ecc71",
    errored: "#e74c3c",
    exited: "#e74c3c",
    external: "#3498db",
};

export function LeapStatusIndicator({ processStatus, connectionStatus, protocolVersion, onStart, onStop }: LeapStatusIndicatorProps) {
    const [expanded, setExpanded] = useState(false);
    const panelRef = useRef<HTMLDivElement>(null);

    // Close panel on outside click
    useEffect(() => {
        if (!expanded) return;
        const handleClick = (e: MouseEvent) => {
            if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
                setExpanded(false);
            }
        };
        document.addEventListener("mousedown", handleClick);
        return () => document.removeEventListener("mousedown", handleClick);
    }, [expanded]);

    const streaming = connectionStatus === "streaming";
    const tooltipText = `Ultraleap: ${CONNECTION_LABELS[connectionStatus]}`;

    const showToggle = processStatus !== "external" && processStatus !== "not-started";
    const isRunning = processStatus === "running" || processStatus === "external";
    const canStart = processStatus === "exited" || processStatus === "errored";

    return (
        <div className="leap-status" ref={panelRef}>
            <button
                className={`leap-status-dot ${streaming ? "connected" : "disconnected"}`}
                onClick={() => setExpanded((prev) => !prev)}
                title={tooltipText}
            />
            {expanded && (
                <div className="leap-status-panel">
                    <div className="leap-status-panel-title">Ultraleap Status</div>
                    <div className="leap-status-row">
                        <span className="leap-status-label">Process</span>
                        <span className="leap-status-value">
                            <span
                                className="leap-status-value-dot"
                                style={{ backgroundColor: PROCESS_COLORS[processStatus] }}
                            />
                            {PROCESS_LABELS[processStatus]}
                        </span>
                    </div>
                    <div className="leap-status-row">
                        <span className="leap-status-label">Connection</span>
                        <span className="leap-status-value">
                            <span
                                className="leap-status-value-dot"
                                style={{ backgroundColor: CONNECTION_COLORS[connectionStatus] }}
                            />
                            {CONNECTION_LABELS[connectionStatus]}
                        </span>
                    </div>
                    {protocolVersion !== null && (
                        <div className="leap-status-row">
                            <span className="leap-status-label">Protocol</span>
                            <span className="leap-status-value">v{protocolVersion}</span>
                        </div>
                    )}
                    {showToggle && (
                        <button
                            className="leap-status-toggle"
                            onClick={isRunning ? onStop : onStart}
                            disabled={!isRunning && !canStart}
                        >
                            {isRunning ? "Stop Server" : "Start Server"}
                        </button>
                    )}
                </div>
            )}
        </div>
    );
}
