import React from "react";
import { Route, Routes } from "react-router-dom";
import { FullPageSketch } from "./routes/fullPageSketch";
import { HomePage } from "./routes/homePage";

import sketches from "./sketches";

const sketchRoutes = sketches.map(sketchClass => {
    const path = `/${sketchClass.id}`;
    return (
        <Route
            key={path}
            path={path}
            element={<FullPageSketch sketchClass={sketchClass} />}
        />
    );
});

export const AppRoutes = () => (
    <Routes>
        {sketchRoutes}
        <Route path="/" element={<HomePage />} />
    </Routes>
);
