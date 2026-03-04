import { useRef, useEffect, useState, ReactNode, useCallback } from "react";
import * as THREE from "three";
import { SketchAudioContext } from "./sketch";
import { AudioContextContext, AudioContextValue } from "./hooks/useAudioContext";

interface AudioContextProviderProps {
    children: ReactNode;
}

/**
 * Provides a shared AudioContext for the entire application.
 *
 * Why this exists:
 * - Browsers limit the number of concurrent AudioContexts (~6 in Chrome)
 * - Three.js uses a global AudioContext reference (THREE.AudioContext.setContext)
 * - Creating/closing AudioContexts on every route change can cause audio glitches
 *   and potential resource exhaustion
 *
 * Audio signal chain:
 *   Sketch audio sources → audioContext.gain → userVolume → destination (speakers)
 *
 * - audioContext.gain: Controlled by individual sketches for their audio levels
 * - userVolume: Controlled by the volume button UI for user mute/unmute
 */
export function AudioContextProvider({ children }: AudioContextProviderProps) {
    const [audioContext, setAudioContext] = useState<SketchAudioContext | null>(null);
    // Use ref for userVolume since we need to mutate it (Web Audio API requirement)
    const userVolumeRef = useRef<GainNode | null>(null);

    useEffect(() => {
        let cancelled = false;

        // Create the AudioContext and register it globally with Three.js.
        // This must happen once at app startup, not per-sketch.
        const ctx = new AudioContext() as SketchAudioContext;
        THREE.AudioContext.setContext(ctx);

        // User volume node - connected directly to speakers.
        // This is what the volume button controls.
        const userVolume = ctx.createGain();
        userVolumeRef.current = userVolume;
        userVolume.connect(ctx.destination);

        // Sketch gain node - sketches connect their audio here.
        // This allows sketches to control their own volume independently.
        const audioContextGain = ctx.createGain();
        ctx.gain = audioContextGain;
        audioContextGain.connect(userVolume);

        queueMicrotask(() => {
            if (!cancelled) {
                setAudioContext(ctx);
            }
        });

        return () => {
            cancelled = true;
            userVolumeRef.current = null;
            ctx.close();
        };
    }, []);

    // Create a stable setUserVolume callback that accesses the ref
    const setUserVolume = useCallback((volume: number) => {
        const userVolume = userVolumeRef.current;
        if (!audioContext || !userVolume) return;

        userVolume.gain.value = volume;

        // Resume or suspend based on volume
        if (volume > 0 && audioContext.state === "suspended") {
            audioContext.resume();
        } else if (volume === 0 && audioContext.state === "running") {
            audioContext.suspend();
        }
    }, [audioContext]);

    // Don't render children until AudioContext is ready
    if (!audioContext) return null;

    // Create the context value
    const value: AudioContextValue = {
        audioContext,
        setUserVolume,
    };

    return (
        <AudioContextContext.Provider value={value}>
            {children}
        </AudioContextContext.Provider>
    );
}
