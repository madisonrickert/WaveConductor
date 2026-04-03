import { useEffect } from "react";
import { Sketch } from "@/sketch/Sketch";

/**
 * Runs init/cleanup for the sketch.
 */
export function useSketchLifecycle(sketch: Sketch) {
  useEffect(() => {
    sketch.init();

    return () => {
      sketch.destroy?.();
    };
  }, [sketch]);
}