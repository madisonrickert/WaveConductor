import React from "react";

export const DEFAULT_NAME = "who are you?";

export function FlameNameInput({ onInput, initialName }: { onInput: (newName: string, isEmpty: boolean) => void; initialName: string }) {
    const handleInput = (event: React.FormEvent<HTMLInputElement>) => {
        const value = event.currentTarget.value;
        const trimmed = value == null ? "" : value.trim();
        onInput(trimmed || DEFAULT_NAME, trimmed === "");
    };

    return (
        <div className="flame-input">
            <input
                defaultValue={initialName}
                placeholder={DEFAULT_NAME}
                maxLength={20}
                onInput={handleInput}
            />
        </div>
    );
}
