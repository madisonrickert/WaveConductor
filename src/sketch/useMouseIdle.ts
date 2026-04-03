import { useEffect, useState } from "react";

const IDLE_TIMEOUT_MS = 3000;

const isTouchDevice = typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;

/**
 * Tracks mouse activity and returns `true` after {@link IDLE_TIMEOUT_MS} of
 * inactivity. Resets on any `mousemove` or `mousedown` event.
 *
 * On touch devices (coarse pointer), always returns `false` so overlay
 * buttons stay visible — there is no cursor to clutter the screen.
 *
 * Used to fade out overlay buttons when the user stops moving the mouse.
 */
export function useMouseIdle(): boolean {
    const [mouseIdle, setMouseIdle] = useState(false);

    useEffect(() => {
        if (isTouchDevice) return;

        let timer = setTimeout(() => setMouseIdle(true), IDLE_TIMEOUT_MS);
        const resetIdle = () => {
            setMouseIdle(false);
            clearTimeout(timer);
            timer = setTimeout(() => setMouseIdle(true), IDLE_TIMEOUT_MS);
        };
        window.addEventListener("mousemove", resetIdle);
        window.addEventListener("mousedown", resetIdle);
        return () => {
            clearTimeout(timer);
            window.removeEventListener("mousemove", resetIdle);
            window.removeEventListener("mousedown", resetIdle);
        };
    }, []);

    return mouseIdle;
}
