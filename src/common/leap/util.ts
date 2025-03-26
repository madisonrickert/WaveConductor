import * as THREE from "three";

import { map } from "../math";

export function mapLeapToThreePosition(canvas: HTMLCanvasElement, position: number[]) {
    const range = [0.2, 0.8];
    // position[0] is left/right; left is negative, right is positive. each unit is one millimeter
    const x = map(position[0], -200, 200, canvas.width * range[0],  canvas.width * range[1]);
    // 40 is about 4cm, 1 inch, to 35cm = 13 inches above
    const y = map(position[1], 350, 40,   canvas.height * range[0], canvas.height * range[1]);
    // put the leap stuff close to the camera so the hand is always visible
    const z = 300;
    return new THREE.Vector3(x, y, z);
}