import { FaHome } from "react-icons/fa";
import { useNavigate } from "react-router";

import "./homeButton.scss";

export function HomeButton() {
    const navigate = useNavigate();

    return (
        <button
            className="overlay-button home-button"
            onClick={() => navigate("/")}
            title="Back to home (Esc)"
        >
            <FaHome />
        </button>
    );
}
