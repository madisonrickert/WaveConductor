import { useEffect } from "react";

import { BaseSketch, UIEventName } from "@/sketch/BaseSketch";
import { useSketchLifecycle } from "@/sketch/useSketchLifecycle";
import { useSketchAnimationLoop } from "@/sketch/useSketchAnimationLoop";
import { useSketchResize } from "@/sketch/useSketchResize";

const noop = () => {};

const EVENT_LISTENER_OPTIONS: Partial<Record<UIEventName, AddEventListenerOptions>> = {
    touchstart: { passive: false },
    touchmove: { passive: false },
};

/**
 * Wires the sketch's event handlers to its canvas element.
 * Touch events use `{ passive: false }` so sketches can call `preventDefault()`.
 */
function useSketchUIEvents(sketch: BaseSketch) {
    useEffect(() => {
        const canvas = sketch.renderer.domElement;
        canvas.setAttribute("tabindex", "1");

        const events = sketch.events;
        if (!events) return;

        const entries = Object.entries(events) as Array<[UIEventName, EventListener]>;
        entries.forEach(([eventName, callback]) => {
            if (callback) {
                const options = EVENT_LISTENER_OPTIONS[eventName];
                canvas.addEventListener(eventName, callback, options);
            }
        });

        return () => {
            entries.forEach(([eventName, callback]) => {
                if (callback) {
                    const options = EVENT_LISTENER_OPTIONS[eventName];
                    canvas.removeEventListener(eventName, callback, options);
                }
            });
        };
    }, [sketch]);
}

/**
 * Drives a sketch's full lifecycle: init/destroy, animation loop, resize
 * handling, and DOM event wiring. Renders the sketch's optional React overlay.
 */
export function SketchRenderer({ sketch }: { sketch: BaseSketch }) {
    useSketchUIEvents(sketch);
    useSketchLifecycle(sketch);

    useSketchResize(sketch.renderer, (width, height) => {
        sketch.resize?.(width, height);
    });

    useSketchAnimationLoop(({ delta }) => {
        try {
            sketch.animate(delta);
        } catch (e) {
            console.error(e);
        }
        // Keep event loop active so Chromium delivers WebSocket messages at full rate.
        // Without this, Chromium throttles WebSocket I/O when the main thread is idle
        // between rAF frames, starving leapjs of hand tracking data (~2-6fps vs ~60fps).
        if (sketch.hasLeapController) setTimeout(noop, 0);
    });

    return (
        <div className="sketch-elements">
            {sketch.render?.()}
        </div>
    );
}
