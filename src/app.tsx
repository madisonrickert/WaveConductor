import React from "react";
import { BrowserRouter } from "react-router-dom";

import { AppRoutes } from "./appRoutes";

// Create a context for reactIconBase
const ReactIconBaseContext = React.createContext({
    className: "fa-icon",
    style: {
        verticalAlign: "text-top",
    },
});

class App extends React.PureComponent<object, object> {
    render() {
        return (
            <BrowserRouter>
                <ReactIconBaseContext.Provider
                    value={{
                        className: "fa-icon",
                        style: {
                            verticalAlign: "text-top",
                        },
                    }}
                >
                    <AppRoutes />
                </ReactIconBaseContext.Provider>
            </BrowserRouter>
        );
    }
}

export default App;
