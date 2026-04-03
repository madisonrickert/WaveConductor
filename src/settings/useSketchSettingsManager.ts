import { useCallback, useEffect, useMemo, useState } from "react";

import { SketchConstructor } from "@/sketch/Sketch";
import { loadSettings, saveSettings } from "@/settings/store";

/**
 * Manages per-sketch settings: loads from localStorage, persists on change,
 * and computes a debounced restart key that triggers sketch re-initialization
 * when a `requiresRestart` setting changes.
 */
export function useSketchSettingsManager(sketchClass: SketchConstructor) {
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

    return { settings, defs, sketchId, setSetting, restartKey };
}
