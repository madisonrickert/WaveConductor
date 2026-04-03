import { useCallback, useEffect, useState } from "react";

import { useAudioContext } from "@/audio/useAudioContext";
import { loadGlobalSettings, saveGlobalSetting } from "@/settings/globalSettings";

/**
 * Manages the user-facing volume toggle: persists preference to localStorage,
 * syncs gain to the shared AudioContext, and suspends/resumes audio on tab
 * visibility changes to save resources when the tab is hidden.
 */
export function useVolume() {
    const { setUserVolume } = useAudioContext();
    const [volumeEnabled, setVolumeEnabled] = useState(() => loadGlobalSettings().volumeEnabled);

    const toggleVolume = useCallback(() => {
        setVolumeEnabled((prev: boolean) => {
            const next = !prev;
            saveGlobalSetting("volumeEnabled", next);
            return next;
        });
    }, []);

    // Sync volume state to the AudioContext gain node
    useEffect(() => {
        setUserVolume(volumeEnabled ? 1 : 0);
    }, [volumeEnabled, setUserVolume]);

    // Pause audio when the browser tab is hidden, resume when visible
    useEffect(() => {
        const handleVisibilityChange = () => {
            setUserVolume(document.hidden ? 0 : (volumeEnabled ? 1 : 0));
        };
        document.addEventListener("visibilitychange", handleVisibilityChange);
        return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
    }, [volumeEnabled, setUserVolume]);

    return { volumeEnabled, toggleVolume };
}
