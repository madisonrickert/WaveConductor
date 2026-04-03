import { useState } from "react";
import { useSketchSettings } from "@/settings/useSketchSettings";
import { GLOBAL_SETTINGS_DEFS, loadGlobalSettings, saveGlobalSetting } from "@/settings/globalSettings";
import { BooleanToggle } from "./inputs/BooleanToggle";
import { ColorInput } from "./inputs/ColorInput";
import { ImageInput } from "./inputs/ImageInput";

import "./advancedSettingsPanel.scss";

export function DevSettingsPanel() {
    const { defs, settings, setSetting } = useSketchSettings();

    const [globalSettings, setGlobalSettings] = useState(loadGlobalSettings);

    const updateGlobalSetting = (key: keyof typeof GLOBAL_SETTINGS_DEFS, value: unknown) => {
        setGlobalSettings(prev => ({ ...prev, [key]: value } as typeof prev));
        saveGlobalSetting(key, value);
    };

    const devEntries = Object.entries(defs).filter(([, def]) => def.category === "dev");
    const globalDevEntries = Object.entries(GLOBAL_SETTINGS_DEFS).filter(([, def]) => def.category === "dev");

    return (
        <div className="overlay-panel advanced-settings-panel">
            <div className="overlay-panel-title">Advanced Settings</div>
            {globalDevEntries.map(([key, def]) => (
                <SettingRow
                    key={`global-${key}`}
                    def={def}
                    value={globalSettings[key as keyof typeof globalSettings]}
                    onChange={(value) => updateGlobalSetting(key as keyof typeof GLOBAL_SETTINGS_DEFS, value)}
                />
            ))}
            {devEntries.map(([key, def]) => (
                <SettingRow
                    key={key}
                    def={def}
                    value={settings[key]}
                    onChange={(value) => setSetting(key, value)}
                />
            ))}
        </div>
    );
}

function SettingInput({ def, value, onChange }: {
    def: { default: unknown; step?: number; min?: number; max?: number; type?: "color" | "image" };
    value: unknown;
    onChange: (value: unknown) => void;
}) {
    if (typeof def.default === "boolean") {
        return <BooleanToggle value={value as boolean} onChange={onChange} />;
    }
    if (def.type === "color") {
        return <ColorInput value={value as string} onChange={onChange} />;
    }
    if (def.type === "image") {
        return <ImageInput value={value as string} onChange={onChange} />;
    }
    if (typeof def.default === "number") {
        return (
            <input
                type="number"
                value={value as number}
                step={def.step}
                min={def.min}
                max={def.max}
                onChange={(e) => {
                    let v = e.target.valueAsNumber || 0;
                    if (def.min != null) v = Math.max(def.min, v);
                    if (def.max != null) v = Math.min(def.max, v);
                    onChange(v);
                }}
            />
        );
    }
    return (
        <input
            type="text"
            value={value as string}
            onChange={(e) => onChange(e.target.value)}
        />
    );
}

function SettingRow({ def, value, onChange }: {
    def: { label: string; requiresRestart?: boolean; default: unknown; step?: number; min?: number; max?: number; type?: "color" | "image" };
    value: unknown;
    onChange: (value: unknown) => void;
}) {
    return (
        <label className="overlay-panel-row advanced-settings-row">
            <span className="overlay-panel-label">
                {def.label}
                {def.requiresRestart && <span className="advanced-settings-restart"> (restart)</span>}
            </span>
            <SettingInput def={def} value={value} onChange={onChange} />
        </label>
    );
}
