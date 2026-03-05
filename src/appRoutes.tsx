import { Route, Routes, useLocation, useNavigate } from "react-router";
import { HomePage } from "./routes/homePage";
import { LicensesPage } from "./routes/licensesPage";
import { SketchComponent } from "./components/sketchComponent";
import { useHotkeys } from 'react-hotkeys-hook';
import { useThrottledNavigate } from "@/common/hooks/useThrottledNavigate";

import { LineSketch, FlameSketch, Dots, Cymatics, Waves } from "./sketches";

const SKETCH_PATHS = ['/line', '/flame', '/dots', '/cymatics', '/waves'];

export const AppRoutes = () => {
    const throttledNavigate = useThrottledNavigate(500);
    const location = useLocation();
    const navigate = useNavigate();

    const navigatePrev = () => {
        const currentIndex = SKETCH_PATHS.indexOf(location.pathname);
        const prevIndex = currentIndex <= 0 ? SKETCH_PATHS.length - 1 : currentIndex - 1;
        throttledNavigate(SKETCH_PATHS[prevIndex]);
    };

    const navigateNext = () => {
        const currentIndex = SKETCH_PATHS.indexOf(location.pathname);
        const nextIndex = currentIndex === -1 || currentIndex >= SKETCH_PATHS.length - 1 ? 0 : currentIndex + 1;
        throttledNavigate(SKETCH_PATHS[nextIndex]);
    };

    useHotkeys('z', navigatePrev);
    useHotkeys('x', navigateNext);
    useHotkeys('left', navigatePrev);
    useHotkeys('right', navigateNext);

    useHotkeys('1', () => throttledNavigate(SKETCH_PATHS[0]));
    useHotkeys('2', () => throttledNavigate(SKETCH_PATHS[1]));
    useHotkeys('3', () => throttledNavigate(SKETCH_PATHS[2]));
    useHotkeys('4', () => throttledNavigate(SKETCH_PATHS[3]));
    useHotkeys('5', () => throttledNavigate(SKETCH_PATHS[4]));

    useHotkeys('escape', () => {
        if (location.pathname !== '/') {
            navigate('/');
        }
    });

    return (
        <Routes>
            <Route path="/line" element={<SketchComponent key="line" sketchClass={LineSketch} />} />
            <Route path="/flame" element={<SketchComponent key="flame" sketchClass={FlameSketch} />} />
            <Route path="/dots" element={<SketchComponent key="dots" sketchClass={Dots} />} />
            <Route path="/cymatics" element={<SketchComponent key="cymatics" sketchClass={Cymatics} />} />
            <Route path="/waves" element={<SketchComponent key="waves" sketchClass={Waves} />} />
            <Route path="/licenses" element={<LicensesPage />} />
            <Route path="/" element={<HomePage />} />
        </Routes>
    );
};
