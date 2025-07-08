import * as Leap from "leapjs";
import { mapLeapToThreePosition } from "@/common/leap/util";
import { HandMesh } from "@/common/leap/handMesh";
import * as THREE from "three";
import LineSketch from ".";

/**
 * Wrapper for the Leap Motion controller inside of the line sketch.
 */
export class LeapAttractorController {
    public controller: Leap.Controller;

    /**
     * Pool containing all hand meshes.
     */
    private _handMeshesGroup = new THREE.Group();

    /**
     * Returns the hand mesh at the given index, creating it if necessary.
     * Adds it to the handMeshesGroup if newly created.
     */
    private getHandMesh(index: number): HandMesh {
        console.log("getHandMesh", index);
        while (this._handMeshesGroup.children.length <= index) {
            const handMesh = new HandMesh();
            handMesh.visible = false;
            this._handMeshesGroup.add(handMesh);
        }
        return this._handMeshesGroup.children[index] as HandMesh;
    }

    constructor(public sketch: LineSketch) {
        this.controller = new Leap.Controller().connect();
        this.sketch.scene.add(this._handMeshesGroup);
    }

    /**
     * Attach the Leap Motion controller to the sketch.
     * This will start listening for Leap Motion frames and handle them.
     */
    attachToLeap() {
        this.controller.on('frame', this.handleFrame);
    }

    /**
     * Handle a Leap Motion frame.
     * @param frame The Leap Motion frame to handle.
     */
    private handleFrame = (frame: Leap.Frame) => {
        if (frame.hands.length > 0) {
            this.sketch.lastRenderedFrame = this.sketch.globalFrame;
        }
        // Hide all hand meshes and zero all Leap attractor powers
        for (const attractor of this.sketch.leapAttractors) {
            attractor.power = 0;
        }
        for (const mesh of this._handMeshesGroup.children) {
            mesh.visible = false;
        }
        frame.hands.filter((hand) => hand.valid).forEach((hand, index) => {
            const position = hand.indexFinger!.bones[3].center();
            const { x, y } = mapLeapToThreePosition(this.sketch.canvas, position);
            if (index === 0) {
                this.sketch.setGravityFocalPoint(x, y);
            }

            // Only update Leap attractors, not mouse attractor
            const attractor = this.sketch.getLeapAttractor(index);
            attractor.x = x;
            attractor.y = y;

            if (hand.indexFinger!.extended) {
                attractor.power = attractor.power * 0.5;
            } else {
                // position[2] goes from -300 to 300
                const wantedPower = Math.pow(7, (-position[2] + 350) / 200);
                attractor.power = attractor.power * 0.5 + wantedPower * 0.5;
            }

            const handMesh = this.getHandMesh(index);
            handMesh.update(this.sketch.canvas, hand);
            handMesh.visible = true;
        });
    }

    /**
     * Detach the Leap Motion controller from the sketch.
     * This will stop listening for Leap Motion frames.
     */
    detachFromLeap() {
        this.controller.removeListener('frame', this.handleFrame);
    }

    lastFrameIsValid() {
        return this.controller.lastFrame.valid;
    }
}
