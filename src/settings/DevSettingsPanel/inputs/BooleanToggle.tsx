export function BooleanToggle({ value, onChange }: {
    value: boolean;
    onChange: (value: boolean) => void;
}) {
    return (
        <button
            type="button"
            role="switch"
            aria-checked={value}
            className={`advanced-settings-toggle-switch ${value ? "on" : ""}`}
            onClick={() => onChange(!value)}
        >
            <span className="advanced-settings-toggle-knob" />
        </button>
    );
}
