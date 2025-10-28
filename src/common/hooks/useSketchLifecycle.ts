import { useEffect } from "react";
import { ISketch } from "@/sketch";

interface SketchLifecycleOptions {
  onInitError?: (error: Error) => void;
  disposeRenderer?: boolean;
}

/**
 * Runs init/cleanup for the sketch and optionally disposes the renderer.
 */
export function useSketchLifecycle(sketch: ISketch) {
  useEffect(() => {
    sketch.init();

    return () => {
      sketch.destroy?.();
      sketch.renderer.dispose();
    };
  }, [sketch]);
}