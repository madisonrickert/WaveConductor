import { useState } from "react";
import { HexColorPicker, HexColorInput } from "react-colorful";

export function ColorInput({ value, onChange }: {
    value: string;
    onChange: (value: string) => void;
}) {
    const [colorOpen, setColorOpen] = useState(false);

    return (
        <div className="advanced-settings-color">
            <button
                type="button"
                className="advanced-settings-color-swatch"
                style={{ backgroundColor: value }}
                onClick={() => setColorOpen(!colorOpen)}
            />
            <HexColorInput
                className="advanced-settings-color-input"
                color={value}
                prefixed
                onChange={(c) => onChange(c)}
            />
            {colorOpen && (
                <div className="advanced-settings-color-popover">
                    <HexColorPicker color={value} onChange={(c) => onChange(c)} />
                </div>
            )}
        </div>
    );
}
