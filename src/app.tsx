import { HashRouter } from "react-router";

import { AppRoutes } from "./appRoutes";
import { AudioContextProvider } from "@/audio/AudioContextProvider";

export default function App() {
    return (
        <HashRouter>
            <AudioContextProvider>
                <AppRoutes />
            </AudioContextProvider>
        </HashRouter>
    );
}
