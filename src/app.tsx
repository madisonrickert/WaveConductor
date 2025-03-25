import React from "react";
import { BrowserRouter } from "react-router-dom";

import { Routes } from "./routes";

class App extends React.PureComponent<{}, {}> {
    static childContextTypes = {
        reactIconBase: true,
    }
    getChildContext() {
        return {
            reactIconBase: {
                className: "fa-icon",
                style: {
                    verticalAlign: "text-top",
                },
            },
        };
    }
    render() {
        return (
            <BrowserRouter>
                <>
                    <Routes />
                </>
            </BrowserRouter>
        );
    }
}

export default App;
