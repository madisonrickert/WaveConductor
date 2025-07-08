import * as Leap from "leapjs";
import { mapLeapToThreePosition } from "@/common/leap/util";
import { HandMesh } from "@/common/leap/handMesh";
import * as THREE from "three";
import LineSketch from ".";


const ATTRACTOR_POWER_ATTACK_SPEED = 0.005;
const ATTRACTOR_POWER_DECAY_SPEED = 0.5;

/**
 * Wrapper for the Leap Motion controller inside of the line sketch.
 */
export class LeapAttractorController {
    public controller = new Leap.Controller();

    /**
     * Pool containing all hand meshes.
     */
    private _handMeshesGroup = new THREE.Group();

    /**
     * Returns the hand mesh at the given index, creating it if necessary.
     * Adds it to the handMeshesGroup if newly created.
     */
    private getHandMesh(index: number): HandMesh {
        while (this._handMeshesGroup.children.length <= index) {
            const handMesh = new HandMesh();
            handMesh.name = `HandMesh ${index}`;
            handMesh.visible = false;
            this._handMeshesGroup.add(handMesh);
        }
        return this._handMeshesGroup.children[index] as HandMesh;
    }

    constructor(public sketch: LineSketch) {
        this.sketch.scene.add(this._handMeshesGroup);
        this.controller
            .connect()
            .on('frame', this.handleFrame);
    }

    /**
     * Handle a Leap Motion frame.
     * @param frame The Leap Motion frame to handle.
     */
    private handleFrame = (frame: Leap.Frame) => {
        if (frame.hands.length > 0) {
            this.sketch.lastRenderedFrame = this.sketch.globalFrame;
        }

        const validHands = frame.hands.filter((hand) => hand.valid);

        // Update only the attractors and meshes for valid hands
        validHands.forEach((hand, index) => {
            const position = hand.palmPosition;
            const { x, y } = mapLeapToThreePosition(this.sketch.canvas, position);
            if (index === 0) {
                this.sketch.setGravityFocalPoint(x, y);
            }

            const attractor = this.sketch.getLeapAttractor(index);
            attractor.x = x;
            attractor.y = y;

            if (hand.grabStrength === 0) {
                attractor.power *= ATTRACTOR_POWER_DECAY_SPEED;
            } else {
                // position[2] goes from -300 to 300
                // hand.grabStrength is between 0 and 1
                const grabComponent = Math.pow(hand.grabStrength, 1.5);
                const depthModulator = Math.pow(5, (-position[2] + 350) / 160);
                const wantedPower = grabComponent * depthModulator;
                attractor.power = attractor.power * (1 - ATTRACTOR_POWER_ATTACK_SPEED) + wantedPower * ATTRACTOR_POWER_ATTACK_SPEED;
            }

            const handMesh = this.getHandMesh(index);
            handMesh.update(this.sketch.canvas, hand);
            handMesh.visible = true;
        });

        // Zero/hide unused attractors and meshes
        for (let i = validHands.length; i < this.sketch.leapAttractors.length; i++) {
            this.sketch.leapAttractors[i].power = 0;
        }
        for (let i = validHands.length; i < this._handMeshesGroup.children.length; i++) {
            this._handMeshesGroup.children[i].visible = false;
        }
    }

    dispose() {
        // this.sketch.scene.remove(this._handMeshesGroup);
        // this._handMeshesGroup.clear();
        this.controller
            .removeListener('frame', this.handleFrame)
            .disconnect();
    }

    lastFrameIsValid() {
        return this.controller.lastFrame.valid;
    }
}
