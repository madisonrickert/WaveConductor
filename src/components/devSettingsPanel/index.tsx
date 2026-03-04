import React from "react";
import { useSketchSettings } from "@/common/hooks/useSketchSettings";

import "./devSettingsPanel.scss";

export function DevSettingsPanel() {
    const { defs, settings, setSetting } = useSketchSettings();

    const devEntries = Object.entries(defs).filter(([, def]) => def.category === "dev");
    if (devEntries.length === 0) return null;

    return (
        <div className="dev-settings-panel">
            <div className="dev-settings-title">Dev Settings</div>
            {devEntries.map(([key, def]) => (
                <label key={key} className="dev-settings-row">
                    <span className="dev-settings-label">
                        {def.label}
                        {def.requiresRestart && <span className="dev-settings-restart"> (restart)</span>}
                    </span>
                    {typeof def.default === "number" ? (
                        <input
                            type="number"
                            value={settings[key] as number}
                            step={def.step}
                            onChange={(e) => setSetting(key, e.target.valueAsNumber || 0)}
                        />
                    ) : (
                        <input
                            type="text"
                            value={settings[key] as string}
                            onChange={(e) => setSetting(key, e.target.value)}
                        />
                    )}
                </label>
            ))}
        </div>
    );
}
