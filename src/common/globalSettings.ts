import { SettingsDefs } from "./sketchSettings";
import { loadSettings, saveSetting } from "./sketchSettingsStore";

const GLOBAL_SETTINGS_ID = "__global";

export const GLOBAL_SETTINGS_DEFS = {
    leapBackground: {
        default: false,
        category: "dev" as const,
        label: "Leap: receive frames when tab is not focused",
        event: "leap-background-changed",
    },
    volumeEnabled: {
        default: true,
        category: "user" as const,
        label: "Volume enabled",
        event: "volume-enabled-changed",
    },
} satisfies Record<string, SettingsDefs[string] & { event?: string }>;

export function loadGlobalSettings() {
    return loadSettings(GLOBAL_SETTINGS_ID, GLOBAL_SETTINGS_DEFS);
}

export function saveGlobalSetting(key: keyof typeof GLOBAL_SETTINGS_DEFS, value: unknown) {
    saveSetting(GLOBAL_SETTINGS_ID, GLOBAL_SETTINGS_DEFS, key, value);
    const def = GLOBAL_SETTINGS_DEFS[key];
    if (def?.event) {
        window.dispatchEvent(new CustomEvent(def.event, { detail: value }));
    }
}
