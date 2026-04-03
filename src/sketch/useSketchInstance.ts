import { useEffect, useRef, useState } from "react";
import * as THREE from "three";

import { Sketch, SketchConstructor, SketchAudioContext } from "@/sketch/Sketch";
import { LeapConnectionStatus } from "@/leap/leapStatus";

export interface SketchInstanceCallbacks {
    setShouldShowScreenSaver: (shouldShow: boolean) => void;
    setConnectionStatus: (status: LeapConnectionStatus) => void;
    setProtocolVersion: (version: number | null) => void;
}

/**
 * Manages the lifecycle of a sketch instance: creates the Three.js renderer,
 * instantiates the sketch, wires callbacks, and cleans up on unmount or restart.
 *
 * Also blocks display sleep on Electron while a sketch is active.
 */
export function useSketchInstance(
    sketchClass: SketchConstructor,
    audioContext: SketchAudioContext,
    restartKey: string,
    callbacks: SketchInstanceCallbacks,
) {
    const [sketch, setSketch] = useState<Sketch | null>(null);
    const containerRef = useRef<HTMLDivElement | null>(null);

    const { setShouldShowScreenSaver, setConnectionStatus, setProtocolVersion } = callbacks;

    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        const renderer = new THREE.WebGLRenderer({
            alpha: true,
            preserveDrawingBuffer: true,
            antialias: true,
        });
        renderer.setSize(container.clientWidth, container.clientHeight);
        container.appendChild(renderer.domElement);

        const sketchInstance = new sketchClass(renderer, audioContext);
        sketchInstance.updateScreenSaverCallback = setShouldShowScreenSaver;
        sketchInstance.updateLeapConnectionCallback = setConnectionStatus;
        sketchInstance.updateLeapProtocolVersionCallback = setProtocolVersion;
        queueMicrotask(() => setSketch(sketchInstance));

        return () => {
            sketchInstance.updateScreenSaverCallback = undefined;
            sketchInstance.updateLeapConnectionCallback = undefined;
            sketchInstance.updateLeapProtocolVersionCallback = undefined;

            renderer.domElement.remove();
            renderer.dispose();
            // AudioContext is shared across sketches — sketch.destroy() handles its own audio nodes
            queueMicrotask(() => setSketch(null));
        };
    }, [sketchClass, audioContext, restartKey, setShouldShowScreenSaver, setConnectionStatus, setProtocolVersion]);

    // Prevent display sleep while a sketch is running (Electron only)
    useEffect(() => {
        const api = window.electronAPI;
        if (!api) return;
        api.startPowerSaveBlocker();
        return () => { api.stopPowerSaveBlocker(); };
    }, []);

    return { sketch, containerRef };
}
