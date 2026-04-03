export interface SettingDef<T = unknown> {
    /** Default value if nothing is persisted */
    default: T;
    /** "user" = shown in normal UI; "dev" = hidden behind Shift+D panel */
    category: "user" | "dev";
    /** Human label for the settings panel */
    label: string;
    /** If true, changing this param triggers sketch re-init */
    requiresRestart?: boolean;
    /** Step increment for number inputs */
    step?: number;
    /** Minimum value for number inputs */
    min?: number;
    /** Maximum value for number inputs */
    max?: number;
    /** Input type hint (e.g. "color" for a color picker, "image" for an image upload) */
    type?: "color" | "image";
}

export type SettingsDefs = Record<string, SettingDef>;

/**
 * Infer the values type from a settings definitions object.
 * Given { name: { default: "hello", ... }, count: { default: 5, ... } }
 * produces { name: string; count: number }
 */
export type SettingsValues<D extends SettingsDefs> = {
    [K in keyof D]: D[K]["default"];
};
