import { Route, Routes, useLocation, useNavigate } from "react-router";
import { HomePage } from "@/routes/homePage/HomePage";
import { LicensesPage } from "@/routes/licensesPage/LicensesPage";
import { SketchView } from "@/sketch/SketchView";
import { useHotkeys } from 'react-hotkeys-hook';
import { useThrottledNavigate } from "@/navigation/useThrottledNavigate";
import { useEdgeSwipeNavigation } from "@/navigation/useEdgeSwipeNavigation";

import { LineSketch, FlameSketch, DotsSketch, CymaticsSketch, WavesSketch } from "@/sketches";

const SKETCH_PATHS = ['/gravity', '/you-niverse', '/fabric', '/cymatics', '/waves'];

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
    useEdgeSwipeNavigation(navigateNext, navigatePrev);

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
            <Route path="/gravity" element={<SketchView key="gravity" sketchClass={LineSketch} />} />
            <Route path="/you-niverse" element={<SketchView key="you-niverse" sketchClass={FlameSketch} />} />
            <Route path="/fabric" element={<SketchView key="fabric" sketchClass={DotsSketch} />} />
            <Route path="/cymatics" element={<SketchView key="cymatics" sketchClass={CymaticsSketch} />} />
            <Route path="/waves" element={<SketchView key="waves" sketchClass={WavesSketch} />} />
            <Route path="/licenses" element={<LicensesPage />} />
            <Route path="/" element={<HomePage />} />
        </Routes>
    );
};
