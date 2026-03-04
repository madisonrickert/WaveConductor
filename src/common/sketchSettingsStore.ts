import { SettingsDefs, SettingsValues } from "./sketchSettings";

const STORAGE_PREFIX = "sketch-settings:";

function getDefaults<D extends SettingsDefs>(defs: D): SettingsValues<D> {
    return Object.fromEntries(
        Object.entries(defs).map(([k, v]) => [k, v.default])
    ) as SettingsValues<D>;
}

/**
 * Load saved settings for a sketch, merged with defaults.
 * Missing keys get the default value.
 */
export function loadSettings<D extends SettingsDefs>(
    sketchId: string,
    defs: D
): SettingsValues<D> {
    const defaults = getDefaults(defs);

    try {
        const raw = localStorage.getItem(`${STORAGE_PREFIX}${sketchId}`);
        if (!raw) return defaults;
        const saved = JSON.parse(raw);
        // Only take keys that exist in defs, fall back to defaults for the rest
        const result = { ...defaults };
        for (const key of Object.keys(defs)) {
            if (key in saved) {
                (result as Record<string, unknown>)[key] = saved[key];
            }
        }
        return result;
    } catch {
        return defaults;
    }
}

/**
 * Save a complete settings object for a sketch.
 */
export function saveSettings(sketchId: string, values: Record<string, unknown>): void {
    localStorage.setItem(`${STORAGE_PREFIX}${sketchId}`, JSON.stringify(values));
}

/**
 * Save a single setting value (loads existing, merges, saves).
 */
export function saveSetting<D extends SettingsDefs>(
    sketchId: string,
    defs: D,
    key: string,
    value: unknown
): void {
    const current = loadSettings(sketchId, defs);
    (current as Record<string, unknown>)[key] = value;
    saveSettings(sketchId, current);
}
