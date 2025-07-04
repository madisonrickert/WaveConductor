import { Route, Routes } from "react-router-dom";
import { HomePage } from "./routes/homePage";
import { SketchComponent } from "./sketchComponent";
import { useHotkeys } from 'react-hotkeys-hook';
import { useNavigate } from "react-router";

import { LineSketch, FlameSketch, Dots, Cymatics, Mito, Waves } from "./sketches";

export const AppRoutes = () => {
    const navigate = useNavigate();

    useHotkeys('z', () => navigate('/line'));
    useHotkeys('x', () => navigate('/cymatics'));

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
