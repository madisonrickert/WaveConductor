import { BrowserRouter } from "react-router";

import { AppRoutes } from "./appRoutes";

export default function App() {
    return (
        <BrowserRouter>
            <AppRoutes />
        </BrowserRouter>
    );
}
