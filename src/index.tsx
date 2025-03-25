import React from "react";
import { createRoot } from "react-dom/client";

import "./monkeypatch";

import App from "./app";

//import "./index.scss";

const rootElement = document.createElement("div");
document.body.appendChild(rootElement);
rootElement.className = "root";

const Error = () => (
    <div style={{width: "800px", fontFamily: "Arial, sans-serif", display: "inline-block", margin: "auto"}}>
        <h2>This is embarassing!</h2>
        <p>Something went wrong, check back later or email me at <a href="mailto:hellocharlien@hotmail.com">hellocharlien (at) hotmail (dot) com</a></p>
    </div>
);

try {
    const root = createRoot(rootElement);
    root.render(<App />);
    const element = document.getElementById("fallback");
    if (element != null) {
        element.remove();
    }
} catch (e) {
    console.error(e);
    const element = document.getElementById("fallback");
    if (element) {
        const fallbackRoot = createRoot(element);
        fallbackRoot.render(<Error />);
    }
}
