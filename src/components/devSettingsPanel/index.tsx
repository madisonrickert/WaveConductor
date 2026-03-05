import { useState } from "react";
import { useSketchSettings } from "@/common/hooks/useSketchSettings";
import { GLOBAL_SETTINGS_DEFS, loadGlobalSettings, saveGlobalSetting } from "@/common/globalSettings";

import "./devSettingsPanel.scss";

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
        <div className="dev-settings-panel">
            <div className="dev-settings-title">Dev Settings</div>
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

function SettingRow({ def, value, onChange }: {
    def: { label: string; requiresRestart?: boolean; default: unknown; step?: number };
    value: unknown;
    onChange: (value: unknown) => void;
}) {
    return (
        <label className="dev-settings-row">
            <span className="dev-settings-label">
                {def.label}
                {def.requiresRestart && <span className="dev-settings-restart"> (restart)</span>}
            </span>
            {typeof def.default === "boolean" ? (
                <input
                    type="checkbox"
                    checked={value as boolean}
                    onChange={(e) => onChange(e.target.checked)}
                />
            ) : typeof def.default === "number" ? (
                <input
                    type="number"
                    value={value as number}
                    step={def.step}
                    onChange={(e) => onChange(e.target.valueAsNumber || 0)}
                />
            ) : (
                <input
                    type="text"
                    value={value as string}
                    onChange={(e) => onChange(e.target.value)}
                />
            )}
        </label>
    );
}
