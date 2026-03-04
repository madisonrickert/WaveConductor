import React, { Component, useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as THREE from "three";
import classnames from "classnames";

import { Sketch, SketchConstructor, UIEventName } from "@/common/sketch";
import { VolumeButton } from "@/components/volumeButton";
import { HandData, HandOverlay } from "@/components/handOverlay";
import { ScreenSaver } from "@/components/screenSaver";
import { DevSettingsPanel } from "@/components/devSettingsPanel";
import { useSketchLifecycle } from "@/common/hooks/useSketchLifecycle";
import { useSketchAnimationLoop } from "@/common/hooks/useSketchAnimationLoop";
import { useSketchResize } from "@/common/hooks/useSketchResize";
import { useAudioContext } from "@/common/hooks/useAudioContext";
import { loadSettings, saveSettings } from "@/common/sketchSettingsStore";
import { SketchSettingsContext } from "@/common/hooks/useSketchSettings";

import "./sketchComponent.scss";

const EVENT_LISTENER_OPTIONS: Partial<Record<UIEventName, AddEventListenerOptions>> = {
    touchstart: { passive: false },
    touchmove: { passive: false },
};

export interface SketchComponentProps extends React.DOMAttributes<HTMLDivElement> {
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
    const [, setTick] = useState(0);

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
        setTick((t) => t + 1);
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

export function SketchComponent({ sketchClass, ...containerProps }: SketchComponentProps) {
    // Use the shared AudioContext from the provider
    const { audioContext, setUserVolume } = useAudioContext();

    const [sketch, setSketch] = useState<Sketch | null>(null);
    const [volumeEnabled, setVolumeEnabled] = useState(() =>
        JSON.parse(window.localStorage.getItem("sketch-volumeEnabled") || "true")
    );
    const [handData, setHandData] = useState<HandData[]>([]);
    const [shouldShowScreenSaver, setShouldShowScreenSaver] = useState(false);
    const [showDevPanel, setShowDevPanel] = useState(false);

    const containerRef = useRef<HTMLDivElement | null>(null);

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

    // Compute a key from requiresRestart settings — when it changes, sketch re-inits
    const restartKey = useMemo(() => {
        return Object.entries(defs)
            .filter(([, def]) => def.requiresRestart)
            .map(([k]) => `${k}=${JSON.stringify(settings[k])}`)
            .join("&");
    }, [defs, settings]);

    // Shift+D to toggle dev settings panel
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
            if (e.shiftKey && e.key === "D") {
                setShowDevPanel(prev => !prev);
            }
        };
        window.addEventListener("keydown", handleKeyDown);
        return () => window.removeEventListener("keydown", handleKeyDown);
    }, []);

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
        sketchInstance.updateHandDataCallback = setHandData;
        queueMicrotask(() => setSketch(sketchInstance));

        return () => {
            // Clear callbacks to prevent stale references
            sketchInstance.updateScreenSaverCallback = undefined;
            sketchInstance.updateHandDataCallback = undefined;

            // Clean up Three.js resources
            renderer.domElement.remove();
            renderer.dispose();

            // Note: We don't close the audioContext here - it's shared across sketches
            // The sketch's destroy() method handles disconnecting its audio nodes

            queueMicrotask(() => setSketch(null));
        };
    }, [sketchClass, audioContext, restartKey]);

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

    const handleVolumeButtonClick = () => {
        setVolumeEnabled((prev: boolean) => {
            const next = !prev;
            window.localStorage.setItem("sketch-volumeEnabled", JSON.stringify(next));
            return next;
        });
    };

    const className = classnames("sketch-component", sketch ? "success" : "loading");

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
                            <HandOverlay hands={handData} />
                        </SketchErrorBoundary>
                    )}
                </div>
                <ScreenSaver shouldShow={shouldShowScreenSaver} />
                <VolumeButton volumeEnabled={volumeEnabled} onClick={handleVolumeButtonClick} />
                {showDevPanel && <DevSettingsPanel />}
            </div>
        </SketchSettingsContext.Provider>
    );
}
