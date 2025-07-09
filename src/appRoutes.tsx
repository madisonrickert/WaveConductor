import { Route, Routes } from "react-router";
import { HomePage } from "./routes/homePage";
import { SketchComponent } from "./components/sketchComponent";
import { useHotkeys } from 'react-hotkeys-hook';
import { useThrottledNavigate } from "@/common/hooks/useThrottledNavigate";

import { LineSketch, FlameSketch, Dots, Cymatics, Mito, Waves } from "./sketches";

export const AppRoutes = () => {
    const throttledNavigate = useThrottledNavigate(500);

    useHotkeys('z', () => throttledNavigate('/line'));
    useHotkeys('x', () => throttledNavigate('/cymatics'));

    return (
        <Routes>
            <Route path="/line" element={<SketchComponent key="line" sketchClass={LineSketch} />} />
            <Route path="/flame" element={<SketchComponent key="flame" sketchClass={FlameSketch} />} />
            <Route path="/dots" element={<SketchComponent key="dots" sketchClass={Dots} />} />
            <Route path="/cymatics" element={<SketchComponent key="cymatics" sketchClass={Cymatics} />} />
            <Route path="/mito" element={<SketchComponent key="mito" sketchClass={Mito} />} />
            <Route path="/waves" element={<SketchComponent key="waves" sketchClass={Waves} />} />
            <Route path="/" element={<HomePage />} />
        </Routes>
    );
};
