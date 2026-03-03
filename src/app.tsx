import { HashRouter } from "react-router";

import { AppRoutes } from "./appRoutes";
import { AudioContextProvider } from "./common/audioContext";

export default function App() {
    return (
        <HashRouter>
            <AudioContextProvider>
                <AppRoutes />
            </AudioContextProvider>
        </HashRouter>
    );
}
