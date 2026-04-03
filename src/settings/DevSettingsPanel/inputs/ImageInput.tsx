import { useRef, useState } from "react";

const MAX_IMAGE_DATA_URL_SIZE = 512 * 1024; // 512KB

export function ImageInput({ value, onChange }: {
    value: string;
    onChange: (value: string) => void;
}) {
    const fileInputRef = useRef<HTMLInputElement>(null);
    const [imageError, setImageError] = useState<string | null>(null);

    const handleImageUpload = (e: React.ChangeEvent<HTMLInputElement>) => {
        const file = e.target.files?.[0];
        if (!file) return;
        setImageError(null);

        const reader = new FileReader();
        reader.onload = () => {
            const dataUrl = reader.result as string;
            if (dataUrl.length > MAX_IMAGE_DATA_URL_SIZE) {
                setImageError(`Image too large (${Math.round(dataUrl.length / 1024)}KB). Max ${MAX_IMAGE_DATA_URL_SIZE / 1024}KB.`);
                return;
            }
            onChange(dataUrl);
        };
        reader.readAsDataURL(file);

        // Reset so the same file can be re-selected
        e.target.value = "";
    };

    return (
        <div className="advanced-settings-image">
            <input
                ref={fileInputRef}
                type="file"
                accept="image/*"
                style={{ display: "none" }}
                onChange={handleImageUpload}
            />
            {value && (
                <img
                    className="advanced-settings-image-preview"
                    src={value}
                    alt="Spawn template"
                />
            )}
            <button
                type="button"
                className="advanced-settings-image-btn"
                onClick={(e) => { e.preventDefault(); fileInputRef.current?.click(); }}
            >
                Upload
            </button>
            {value && (
                <button
                    type="button"
                    className="advanced-settings-image-btn advanced-settings-image-reset"
                    onClick={(e) => { e.preventDefault(); onChange(""); setImageError(null); }}
                >
                    Reset
                </button>
            )}
            {imageError && <span className="advanced-settings-image-error">{imageError}</span>}
        </div>
    );
}
