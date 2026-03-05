import * as THREE from "three";

import { applyBranch, Branch } from "./branch";
import { VARIATIONS } from "./transforms";
import { UpdateVisitor } from "./updateVisitor";

const tempPoint = new THREE.Vector3();
const tempColor = new THREE.Color();

export class SuperPoint {
    public children?: SuperPoint[];
    public lastPoint: THREE.Vector3 = new THREE.Vector3();
    private static globalSubtreeIterationIndex = 0;

    /** Tracks the next available geometry buffer slot across all SuperPoints sharing a geometry. Reset before creating a new tree. */
    public static nextSlot = 0;
    /** The index in the shared geometry buffer where this SuperPoint's position/color are stored. */
    public readonly slot: number;

    constructor(
        public point: THREE.Vector3,
        public color: THREE.Color,
        public rootGeometry: THREE.BufferGeometry,
        public branches: Branch[],
    ) {
        this.lastPoint.copy(point);

        this.slot = SuperPoint.nextSlot++;

        const positionArray = (rootGeometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
        const colorArray = (rootGeometry.attributes.color as THREE.BufferAttribute).array as Float32Array;

        const offset = this.slot * 3;
        positionArray[offset] = point.x;
        positionArray[offset + 1] = point.y;
        positionArray[offset + 2] = point.z;
        colorArray[offset] = color.r;
        colorArray[offset + 1] = color.g;
        colorArray[offset + 2] = color.b;
    }

    public updateSubtree(depth: number, shouldLerp: boolean, visitors: UpdateVisitor[], posArr: Float32Array, colArr: Float32Array) {
        if (depth === 0) { return; }

        if (this.children === undefined) {
            this.children = this.branches.map(() => {
                return new SuperPoint(
                    new THREE.Vector3(),
                    new THREE.Color(0, 0, 0),
                    this.rootGeometry,
                    this.branches,
                );
            });
        }

        for (let idx = 0, l = this.children.length; idx < l; idx++) {
            SuperPoint.globalSubtreeIterationIndex++;
            const child: SuperPoint = this.children[idx];
            const branch = this.branches[idx];
            // reset the child's position to your updated position so it's ready to get stepped
            child.lastPoint.copy(child.point);

            tempColor.copy(this.color);
            if (shouldLerp) {
                tempPoint.copy(this.point);
                applyBranch(branch, tempPoint, tempColor);
                child.point.lerp(tempPoint, 0.8);
            } else {
                child.point.copy(this.point);
                applyBranch(branch, child.point, tempColor);
            }
            child.color.lerp(tempColor, 0.75);

            // take far away points and move them into the center again to keep points from getting too out of hand
            if (child.point.lengthSq() > 50 * 50) {
                VARIATIONS.Spherical(child.point);
            }

            const childOffset = child.slot * 3;
            posArr[childOffset] = child.point.x;
            posArr[childOffset + 1] = child.point.y;
            posArr[childOffset + 2] = child.point.z;
            colArr[childOffset] = child.color.r;
            colArr[childOffset + 1] = child.color.g;
            colArr[childOffset + 2] = child.color.b;

            if (visitors.length > 0 && SuperPoint.globalSubtreeIterationIndex % 307 === 0) {
                for (const v of visitors) {
                    v.visit(child);
                }
            }

            child.updateSubtree(depth - 1, shouldLerp, visitors, posArr, colArr);
        }
    }

    public recalculate(
        initialX: number,
        initialY: number,
        initialZ: number,
        depth: number,
        shouldLerp: boolean,
        visitors: UpdateVisitor[]) {
        SuperPoint.globalSubtreeIterationIndex = 0;
        this.point.set(initialX, initialY, initialZ);

        const posArr = (this.rootGeometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
        const colArr = (this.rootGeometry.attributes.color as THREE.BufferAttribute).array as Float32Array;

        // Write root position back to buffer
        const rootOffset = this.slot * 3;
        posArr[rootOffset] = this.point.x;
        posArr[rootOffset + 1] = this.point.y;
        posArr[rootOffset + 2] = this.point.z;

        this.updateSubtree(depth, shouldLerp, visitors, posArr, colArr);

        this.rootGeometry.setDrawRange(0, SuperPoint.nextSlot);
        (this.rootGeometry.attributes.position as THREE.BufferAttribute).needsUpdate = true;
        (this.rootGeometry.attributes.color as THREE.BufferAttribute).needsUpdate = true;
    }
}
