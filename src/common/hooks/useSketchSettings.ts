import { createContext, useContext } from "react";
import { SettingsDefs, SettingsValues } from "../sketchSettings";

export interface SketchSettingsContextValue<D extends SettingsDefs = SettingsDefs> {
    settings: SettingsValues<D>;
    defs: D;
    sketchId: string;
    setSetting: (key: string, value: unknown) => void;
}

export const SketchSettingsContext = createContext<SketchSettingsContextValue | null>(null);

export function useSketchSettings<D extends SettingsDefs = SettingsDefs>(): SketchSettingsContextValue<D> {
    const value = useContext(SketchSettingsContext);
    if (!value) {
        throw new Error("useSketchSettings must be used within a SketchSettingsProvider");
    }
    return value as SketchSettingsContextValue<D>;
}
