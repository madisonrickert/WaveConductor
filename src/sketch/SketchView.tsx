import { useMemo, useState } from "react";
import classnames from "classnames";

import { SketchConstructor } from "@/sketch/Sketch";
import { useAudioContext } from "@/audio/useAudioContext";
import { useVolume } from "@/audio/useVolume";
import { useSketchSettingsManager } from "@/settings/useSketchSettingsManager";
import { SketchSettingsContext } from "@/settings/useSketchSettings";
import { useLeapStatus } from "@/leap/useLeapStatus";
import { useSketchInstance } from "@/sketch/useSketchInstance";
import { useMouseIdle } from "@/sketch/useMouseIdle";
import { SketchRenderer } from "@/sketch/SketchRenderer";
import { SketchErrorBoundary } from "@/sketch/SketchErrorBoundary";
import { SketchOverlay } from "@/sketch/SketchOverlay";

import "./sketchView.scss";

export interface SketchViewProps {
    sketchClass: SketchConstructor;
}

/**
 * Top-level view for a single sketch. Composes the sketch instance, settings,
 * audio, and UI overlay without owning any state management logic directly —
 * each concern is delegated to a dedicated hook or child component.
 */
export function SketchView({ sketchClass }: SketchViewProps) {
    const { audioContext } = useAudioContext();
    const { settings, defs, sketchId, setSetting, restartKey } = useSketchSettingsManager(sketchClass);
    const { volumeEnabled, toggleVolume } = useVolume();
    const [shouldShowScreenSaver, setShouldShowScreenSaver] = useState(false);
    const leapStatus = useLeapStatus();
    const mouseIdle = useMouseIdle();

    const { sketch, containerRef } = useSketchInstance(sketchClass, audioContext, restartKey, {
        setShouldShowScreenSaver,
        setConnectionStatus: leapStatus.setConnectionStatus,
        setProtocolVersion: leapStatus.setProtocolVersion,
    });

    const settingsContext = useMemo(
        () => ({ settings, defs, sketchId, setSetting }),
        [settings, defs, sketchId, setSetting],
    );

    return (
        <SketchSettingsContext.Provider value={settingsContext}>
            <div
                id={sketchClass.id}
                className={classnames("sketch-component", sketch ? "success" : "loading", mouseIdle && "mouse-idle")}
                ref={containerRef}
            >
                <div style={{ position: "relative" }}>
                    {sketch && (
                        <SketchErrorBoundary>
                            <SketchRenderer key={sketchClass.id} sketch={sketch} />
                        </SketchErrorBoundary>
                    )}
                </div>
                <SketchOverlay
                    volumeEnabled={volumeEnabled}
                    onToggleVolume={toggleVolume}
                    shouldShowScreenSaver={shouldShowScreenSaver}
                    leapStatus={leapStatus}
                />
            </div>
        </SketchSettingsContext.Provider>
    );
}
