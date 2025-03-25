import React from "react";
import { Redirect, Route, Switch } from "react-router-dom";
import { FullPageSketch } from "./routes/fullPageSketch";
import { HomePage } from "./routes/homePage";

import sketches from "./sketches";

const sketchRoutes = sketches.map(sketchClass => {
    const path = `/${sketchClass.id}`;
    return (
        <Route
            key={path}
            path={path}
            component={() => <FullPageSketch sketchClass={sketchClass} />}
        />
    );
});

export const Routes = () => (
    <Switch>
        {sketchRoutes}
        <Route path="/" component={HomePage} />
    </Switch>
);
