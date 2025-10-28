import { useEffect, useRef } from "react";

/**
 * Drives the requestAnimationFrame loop for the sketch.
 */
export function useSketchAnimationLoop(
    onFrame: (info: { delta: number; timestamp: number }) => void
) {
  const lastFrameIdRef = useRef<number | null>(null);
  const lastTimestampRef = useRef<number | null>(null);

  const animate = (timestamp: number) => {
    if (lastTimestampRef.current !== null) {
        const delta = timestamp - lastTimestampRef.current;
        onFrame({ delta, timestamp });
    }
    lastTimestampRef.current = timestamp;
    lastFrameIdRef.current = requestAnimationFrame(animate);
  }

  useEffect(() => {
    lastFrameIdRef.current = requestAnimationFrame(animate);

    return () => {
      if (lastFrameIdRef.current) {
        cancelAnimationFrame(lastFrameIdRef.current);
      }
      lastTimestampRef.current = null;
    };
  }, []);
}
