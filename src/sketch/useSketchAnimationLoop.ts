import { useEffect, useLayoutEffect, useRef } from "react";

export interface SketchAnimationFrameInfo {
  delta: number;
  timestamp: number;
}

/**
 * Drives the requestAnimationFrame loop for the sketch.
 */
export function useSketchAnimationLoop(
    onFrame: (info: SketchAnimationFrameInfo) => void
) {
  const lastTimestampRef = useRef<number | null>(null);
  const onFrameRef = useRef(onFrame);

  // Update the callback ref before layout effects
  useLayoutEffect(() => {
    onFrameRef.current = onFrame;
  });

  useEffect(() => {
    let frameId: number;

    const animate = (timestamp: number) => {
      if (lastTimestampRef.current !== null) {
        const delta = timestamp - lastTimestampRef.current;
        onFrameRef.current({ delta, timestamp });
      }
      lastTimestampRef.current = timestamp;
      frameId = requestAnimationFrame(animate);
    };

    frameId = requestAnimationFrame(animate);

    return () => {
      cancelAnimationFrame(frameId);
      lastTimestampRef.current = null;
    };
  }, []);
}
