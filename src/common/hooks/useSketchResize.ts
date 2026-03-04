import { useEffect, useLayoutEffect, useRef } from "react";
import * as THREE from "three";

/**
 * Keeps the renderer size in sync with its parent element and forwards resize events to the sketch.
 */
export function useSketchResize(
  renderer: THREE.WebGLRenderer,
  onResize: (width: number, height: number) => void
) {
  const onResizeRef = useRef(onResize);

  useLayoutEffect(() => {
    onResizeRef.current = onResize;
  });

  useEffect(() => {
    const canvas = renderer.domElement;
    const parent = canvas.parentElement;
    if (!parent) return;

    const resize = () => {
      renderer.setSize(parent.clientWidth, parent.clientHeight);
      onResizeRef.current(canvas.width, canvas.height);
    };

    resize(); // initial

    const observer = new ResizeObserver(resize);
    observer.observe(parent);

    return () => {
      observer.disconnect();
    };
  }, [renderer]);
}