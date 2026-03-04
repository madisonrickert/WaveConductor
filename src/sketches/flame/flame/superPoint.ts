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

        const positionAttribute = rootGeometry.attributes.position as THREE.BufferAttribute;
        const colorAttribute = rootGeometry.attributes.color as THREE.BufferAttribute;

        const positionArray = positionAttribute.array as Float32Array;
        const colorArray = colorAttribute.array as Float32Array;

        positionArray.set([point.x, point.y, point.z], this.slot * 3);
        colorArray.set([color.r, color.g, color.b], this.slot * 3);

        positionAttribute.needsUpdate = true;
        colorAttribute.needsUpdate = true;
    }

    public updateSubtree(depth: number, shouldLerp: boolean, ...visitors: UpdateVisitor[]) {
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

        const posArr = (this.rootGeometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
        const colArr = (this.rootGeometry.attributes.color as THREE.BufferAttribute).array as Float32Array;

        for (let idx = 0, l = this.children.length; idx < l; idx++) {
            SuperPoint.globalSubtreeIterationIndex++;
            const child = this.children[idx];
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

            posArr.set([child.point.x, child.point.y, child.point.z], child.slot * 3);
            colArr.set([child.color.r, child.color.g, child.color.b], child.slot * 3);

            if (SuperPoint.globalSubtreeIterationIndex % 307 === 0) {
                for (const v of visitors) {
                    v.visit(child);
                }
            }

            child.updateSubtree(depth - 1, shouldLerp, ...visitors);
        }
    }

    public recalculate(
        initialX: number,
        initialY: number,
        initialZ: number,
        depth: number,
        shouldLerp: boolean,
        ...visitors: UpdateVisitor[]) {
        SuperPoint.globalSubtreeIterationIndex = 0;
        this.point.set(initialX, initialY, initialZ);

        // Write root position back to buffer
        const posArr = (this.rootGeometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
        posArr.set([this.point.x, this.point.y, this.point.z], this.slot * 3);

        this.updateSubtree(depth, shouldLerp, ...visitors);

        this.rootGeometry.setDrawRange(0, SuperPoint.nextSlot);
        (this.rootGeometry.attributes.position as THREE.BufferAttribute).needsUpdate = true;
        (this.rootGeometry.attributes.color as THREE.BufferAttribute).needsUpdate = true;
    }
}
