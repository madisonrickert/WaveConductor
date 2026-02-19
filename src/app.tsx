import { BrowserRouter } from "react-router";

import { AppRoutes } from "./appRoutes";
import { AudioContextProvider } from "./common/audioContext";

export default function App() {
    return (
        <BrowserRouter>
            <AudioContextProvider>
                <AppRoutes />
            </AudioContextProvider>
        </BrowserRouter>
    );
}
