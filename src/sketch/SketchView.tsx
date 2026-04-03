import React, { Component, useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as THREE from "three";
import classnames from "classnames";

import { Sketch, SketchConstructor, UIEventName } from "@/sketch/Sketch";
import { FaCog } from "react-icons/fa";
import { VolumeButton } from "@/ui/volumeButton/VolumeButton";
import { ScreenSaver } from "@/ui/screenSaver/ScreenSaver";
import { DevSettingsPanel } from "@/settings/DevSettingsPanel/DevSettingsPanel";
import { LeapStatusIndicator } from "@/leap/LeapStatusIndicator/LeapStatusIndicator";
import { useSketchLifecycle } from "@/sketch/useSketchLifecycle";
import { useSketchAnimationLoop } from "@/sketch/useSketchAnimationLoop";
import { useSketchResize } from "@/sketch/useSketchResize";
import { useAudioContext } from "@/audio/useAudioContext";
import { loadSettings, saveSettings } from "@/settings/store";
import { SketchSettingsContext } from "@/settings/useSketchSettings";
import { loadGlobalSettings, saveGlobalSetting } from "@/settings/globalSettings";
import { useLeapStatus } from "@/leap/useLeapStatus";
import { HomeButton } from "@/ui/homeButton/HomeButton";

import "./sketchView.scss";

const noop = () => {};

const EVENT_LISTENER_OPTIONS: Partial<Record<UIEventName, AddEventListenerOptions>> = {
    touchstart: { passive: false },
    touchmove: { passive: false },
};

export interface SketchViewProps extends React.DOMAttributes<HTMLDivElement> {
    sketchClass: SketchConstructor;
}

function useSketchUIEvents(sketch: Sketch) {
    useEffect(() => {
        const canvas = sketch.renderer.domElement;
        canvas.setAttribute("tabindex", "1");

        const events = sketch.events;
        if (!events) return;

        const entries = Object.entries(events) as Array<[UIEventName, EventListener]>;
        entries.forEach(([eventName, callback]) => {
            if (callback) {
                const options = EVENT_LISTENER_OPTIONS[eventName];
                canvas.addEventListener(eventName, callback, options);
            }
        });

        return () => {
            entries.forEach(([eventName, callback]) => {
                if (callback) {
                    const options = EVENT_LISTENER_OPTIONS[eventName];
                    canvas.removeEventListener(eventName, callback, options);
                }
            });
        };
    }, [sketch]);
}

function SketchRenderer({ sketch }: { sketch: Sketch }) {
    // const [, setTick] = useState(0);

    useSketchUIEvents(sketch);
    useSketchLifecycle(sketch);

    useSketchResize(sketch.renderer, (width, height) => {
        sketch.resize?.(width, height);
    });

    useSketchAnimationLoop(({ delta }) => {
        try {
            sketch.animate(delta);
        } catch (e) {
            console.error(e);
        }
        // Force re-render to update sketch.render()
        // setTick((t) => t + 1);
        // Keep event loop active so Chromium delivers WebSocket messages at full rate.
        // Without this, Chromium throttles WebSocket I/O when the main thread is idle
        // between rAF frames, starving leapjs of hand tracking data (~2-6fps vs ~60fps).
        setTimeout(noop, 0);
    });

    return (
        <div className="sketch-elements">
            {sketch.render?.()}
        </div>
    );
}

interface SketchErrorBoundaryState {
    error: Error | null;
}

class SketchErrorBoundary extends Component<{ children: React.ReactNode }, SketchErrorBoundaryState> {
    state: SketchErrorBoundaryState = { error: null };

    static getDerivedStateFromError(error: Error) {
        return { error };
    }

    render() {
        if (this.state.error) {
            return (
                <div className="sketch-error">
                    <p>Something went wrong rendering this sketch.</p>
                    <pre>{this.state.error.message}</pre>
                </div>
            );
        }
        return this.props.children;
    }
}

export function SketchView({ sketchClass, ...containerProps }: SketchViewProps) {
    // Use the shared AudioContext from the provider
    const { audioContext, setUserVolume } = useAudioContext();

    const [sketch, setSketch] = useState<Sketch | null>(null);
    const [volumeEnabled, setVolumeEnabled] = useState(() => loadGlobalSettings().volumeEnabled);
    const [shouldShowScreenSaver, setShouldShowScreenSaver] = useState(false);
    const [showDevPanel, setShowDevPanel] = useState(false);
    const { processStatus, connectionStatus, setConnectionStatus, protocolVersion, setProtocolVersion, startProcess, stopProcess } = useLeapStatus();

    const containerRef = useRef<HTMLDivElement | null>(null);
    const devPanelRef = useRef<HTMLDivElement | null>(null);

    // Settings management
    const sketchId = sketchClass.id ?? sketchClass.name;
    const defs = useMemo(() => sketchClass.settings ?? {}, [sketchClass.settings]);

    const [settings, setSettingsState] = useState(() => loadSettings(sketchId, defs));

    const setSetting = useCallback((key: string, value: unknown) => {
        setSettingsState(prev => {
            const next = { ...prev, [key]: value };
            saveSettings(sketchId, next);
            return next;
        });
    }, [sketchId]);

    // Compute a key from requiresRestart settings — when it changes, sketch re-inits.
    // Debounced so rapid changes (e.g. dragging a color picker) don't spam re-inits.
    const rawRestartKey = useMemo(() => {
        return Object.entries(defs)
            .filter(([, def]) => def.requiresRestart)
            .map(([k]) => `${k}=${JSON.stringify(settings[k])}`)
            .join("&");
    }, [defs, settings]);

    const [restartKey, setRestartKey] = useState(rawRestartKey);
    useEffect(() => {
        const timer = setTimeout(() => setRestartKey(rawRestartKey), 300);
        return () => clearTimeout(timer);
    }, [rawRestartKey]);

    const toggleVolume = useCallback(() => {
        setVolumeEnabled((prev: boolean) => {
            const next = !prev;
            saveGlobalSetting("volumeEnabled", next);
            return next;
        });
    }, []);

    // Keyboard shortcuts
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
            if (e.shiftKey && e.key === "D") {
                setShowDevPanel(prev => !prev);
            }
            if (e.key === "v" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
                toggleVolume();
            }
        };
        window.addEventListener("keydown", handleKeyDown);
        return () => window.removeEventListener("keydown", handleKeyDown);
    }, [toggleVolume]);

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

    // Initialize sketch when container mounts (or restartKey changes)
    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        // Create renderer
        const renderer = new THREE.WebGLRenderer({
            alpha: true,
            preserveDrawingBuffer: true,
            antialias: true
        });
        renderer.setSize(container.clientWidth, container.clientHeight);
        container.appendChild(renderer.domElement);

        // Create sketch instance using the shared audioContext
        const sketchInstance = new sketchClass(renderer, audioContext);
        sketchInstance.updateScreenSaverCallback = setShouldShowScreenSaver;
        sketchInstance.updateLeapConnectionCallback = setConnectionStatus;
        sketchInstance.updateLeapProtocolVersionCallback = setProtocolVersion;
        queueMicrotask(() => setSketch(sketchInstance));

        return () => {
            // Clear callbacks to prevent stale references
            sketchInstance.updateScreenSaverCallback = undefined;
            sketchInstance.updateLeapConnectionCallback = undefined;
            sketchInstance.updateLeapProtocolVersionCallback = undefined;

            // Clean up Three.js resources
            renderer.domElement.remove();
            renderer.dispose();

            // Note: We don't close the audioContext here - it's shared across sketches
            // The sketch's destroy() method handles disconnecting its audio nodes

            queueMicrotask(() => setSketch(null));
        };
    }, [sketchClass, audioContext, restartKey, setConnectionStatus, setProtocolVersion]);

    // Prevent display sleep while a sketch is running (Electron only)
    useEffect(() => {
        const api = window.electronAPI;
        if (!api) return;
        api.startPowerSaveBlocker();
        return () => { api.stopPowerSaveBlocker(); };
    }, []);

    // Sync volume changes to the shared AudioContext
    useEffect(() => {
        setUserVolume(volumeEnabled ? 1 : 0);
    }, [volumeEnabled, setUserVolume]);

    // Handle visibility changes (pause audio when tab is hidden)
    useEffect(() => {
        const handleVisibilityChange = () => {
            // When tab is hidden, suspend to save resources
            // When visible again, only resume if user has volume enabled
            setUserVolume(document.hidden ? 0 : (volumeEnabled ? 1 : 0));
        };

        document.addEventListener("visibilitychange", handleVisibilityChange);
        return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
    }, [volumeEnabled, setUserVolume]);

    const handleVolumeButtonClick = toggleVolume;

    // Fade out overlay buttons after mouse inactivity
    const [mouseIdle, setMouseIdle] = useState(false);
    useEffect(() => {
        let timer = setTimeout(() => setMouseIdle(true), 3000);
        const resetIdle = () => {
            setMouseIdle(false);
            clearTimeout(timer);
            timer = setTimeout(() => setMouseIdle(true), 3000);
        };
        window.addEventListener("mousemove", resetIdle);
        window.addEventListener("mousedown", resetIdle);
        return () => {
            clearTimeout(timer);
            window.removeEventListener("mousemove", resetIdle);
            window.removeEventListener("mousedown", resetIdle);
        };
    }, []);

    const className = classnames("sketch-component", sketch ? "success" : "loading", mouseIdle && "mouse-idle");

    const settingsContextValue = useMemo(() => ({
        settings, defs, sketchId, setSetting
    }), [settings, defs, sketchId, setSetting]);

    return (
        <SketchSettingsContext.Provider value={settingsContextValue}>
            <div {...containerProps} id={sketchClass.id} className={className} ref={containerRef}>
                <div style={{ position: "relative" }}>
                    {sketch && (
                        <SketchErrorBoundary>
                            <SketchRenderer key={sketchClass.id} sketch={sketch} />
                        </SketchErrorBoundary>
                    )}
                </div>
                <ScreenSaver shouldShow={shouldShowScreenSaver} />
                <HomeButton />
                <VolumeButton volumeEnabled={volumeEnabled} onClick={handleVolumeButtonClick} />
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
                <LeapStatusIndicator
                    processStatus={processStatus}
                    connectionStatus={connectionStatus}
                    protocolVersion={protocolVersion}
                    onStart={startProcess}
                    onStop={stopProcess}
                />
            </div>
        </SketchSettingsContext.Provider>
    );
}
