import { useEffect } from "react";
import * as THREE from "three";

/**
 * Keeps the renderer size in sync with its parent element and forwards resize events to the sketch.
 */
export function useSketchResize(
  renderer: THREE.WebGLRenderer,
  onResize: (width: number, height: number) => void
) {
  const resize = () => {
    const canvas = renderer.domElement;
    const parent = canvas.parentElement;
    if (!parent) return;
    renderer.setSize(parent.clientWidth, parent.clientHeight);
    onResize(canvas.width, canvas.height);
  };

  useEffect(() => {
    resize(); // initial
    window.addEventListener("resize", resize);

    return () => {
      window.removeEventListener("resize", resize);
    };
  }, []);
}