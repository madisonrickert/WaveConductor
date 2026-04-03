import { useEffect, useRef, useState } from "react";
import { FaCog } from "react-icons/fa";

import { VolumeButton } from "@/ui/volumeButton/VolumeButton";
import { DismissMethod, ScreenSaver } from "@/ui/screenSaver/ScreenSaver";
import { DevSettingsPanel } from "@/settings/DevSettingsPanel/DevSettingsPanel";
import { LeapStatusIndicator } from "@/leap/LeapStatusIndicator/LeapStatusIndicator";
import { HomeButton } from "@/ui/homeButton/HomeButton";
import { useLeapStatus } from "@/leap/useLeapStatus";
import { LeapConnectionStatus } from "@/leap/leapStatus";
import { isTouchDevice } from "@/device";

function getDismissMethod(connectionStatus: LeapConnectionStatus): DismissMethod {
    if (isTouchDevice) return "touch";
    if (connectionStatus !== "disconnected") return "motion";
    return "mouse";
}

export interface SketchOverlayProps {
    volumeEnabled: boolean;
    onToggleVolume: () => void;
    shouldShowScreenSaver: boolean;
    leapStatus: ReturnType<typeof useLeapStatus>;
}

/**
 * UI chrome rendered on top of the sketch canvas: navigation, volume,
 * screensaver overlay, dev settings panel, and Leap Motion status.
 *
 * Handles its own keyboard shortcuts (`v` for volume, `Shift+D` for dev panel)
 * Mouse-idle fade is handled by the parent via CSS class on the container.
 */
export function SketchOverlay({ volumeEnabled, onToggleVolume, shouldShowScreenSaver, leapStatus }: SketchOverlayProps) {
    const [showDevPanel, setShowDevPanel] = useState(false);
    const devPanelRef = useRef<HTMLDivElement | null>(null);

    // Keyboard shortcuts
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
            if (e.shiftKey && e.key === "D") {
                setShowDevPanel(prev => !prev);
            }
            if (e.key === "v" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
                onToggleVolume();
            }
        };
        window.addEventListener("keydown", handleKeyDown);
        return () => window.removeEventListener("keydown", handleKeyDown);
    }, [onToggleVolume]);

    // Close settings panel on click outside
    useEffect(() => {
        if (!showDevPanel) return;
        const handleMouseDown = (e: MouseEvent) => {
            if (devPanelRef.current && !devPanelRef.current.contains(e.target as Node)) {
                setShowDevPanel(false);
            }
        };
        document.addEventListener("mousedown", handleMouseDown);
        return () => document.removeEventListener("mousedown", handleMouseDown);
    }, [showDevPanel]);

    return (
        <>
            <ScreenSaver shouldShow={shouldShowScreenSaver} dismissMethod={getDismissMethod(leapStatus.connectionStatus)} />
            <HomeButton />
            <VolumeButton volumeEnabled={volumeEnabled} onClick={onToggleVolume} />
            <div ref={devPanelRef}>
                <button
                    className="overlay-button advanced-settings-toggle"
                    onClick={() => setShowDevPanel(prev => !prev)}
                    title="Advanced Settings (Shift+D)"
                >
                    <FaCog />
                </button>
                {showDevPanel && <DevSettingsPanel />}
            </div>
            {!isTouchDevice && <LeapStatusIndicator
                processStatus={leapStatus.processStatus}
                connectionStatus={leapStatus.connectionStatus}
                protocolVersion={leapStatus.protocolVersion}
                onStart={leapStatus.startProcess}
                onStop={leapStatus.stopProcess}
            />}
        </>
    );
}
