import { createContext, useContext } from "react";
import { SketchAudioContext } from "@/sketch";

export interface AudioContextValue {
    audioContext: SketchAudioContext;
    /**
     * Set the user volume (0 = muted, 1 = full volume).
     * Also handles suspending/resuming the AudioContext.
     */
    setUserVolume: (volume: number) => void;
}

export const AudioContextContext = createContext<AudioContextValue | null>(null);

/**
 * Hook to access the shared AudioContext.
 * Must be used within an AudioContextProvider.
 *
 * Returns:
 * - audioContext: The Web Audio AudioContext, extended with a `gain` property
 *   for sketch-level volume control
 * - setUserVolume: Function to set user volume (0 = muted, 1 = full)
 */
export function useAudioContext(): AudioContextValue {
    const value = useContext(AudioContextContext);
    if (!value) {
        throw new Error("useAudioContext must be used within AudioContextProvider");
    }
    return value;
}
