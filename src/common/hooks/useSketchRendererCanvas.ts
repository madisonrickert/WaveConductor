import { useCallback, useState } from "react";

/**
 * Ensures the sketch canvas is keyboard-focusable and returns it for consumers.
 */
export function useSketchRendererCanvas() {
  const [canvas] = useState(() => {
    const el = document.createElement("canvas");
    el.setAttribute("tabindex", el.getAttribute("tabindex") ?? "1");
    return el;
  });

  const focusCanvas = useCallback(() => {
    canvas.focus();
  }, [canvas]);

  return { canvas, focusCanvas };
}
