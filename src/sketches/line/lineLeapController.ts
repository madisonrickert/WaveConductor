import * as Leap from "leapjs";
import { mapLeapToThreePosition } from "@/common/leap/util";
import { HandMesh } from "@/common/leap/handMesh";
import LineSketch from ".";

export function initLeap(sketch: LineSketch): Leap.Controller {
	const controller = Leap.loop((frame: Leap.Frame) => {
        if (frame.hands.length > 0) {
            sketch.lastRenderedFrame = sketch.globalFrame;
        }
        for(const attractor of sketch.attractors) {
            if (attractor.handMesh != null) {
                attractor.handMesh.visible = false;
            }
            attractor.mesh.visible = false;
            attractor.power = 0;
        }
        frame.hands.filter((hand) => hand.valid).forEach((hand, index) => {
            const position = hand.indexFinger.bones[3].center();

            const {x, y} = mapLeapToThreePosition(sketch.canvas, position);
            sketch.setMousePosition(x, y);

            const attractor = sketch.attractors[index];
            attractor.x = x;
            attractor.y = y;
            attractor.mesh.position.x = x;
            attractor.mesh.position.y = y;

            attractor.mesh.visible = true;
            if (hand.indexFinger.extended) {
                // position[2] goes from -300 to 300
                const wantedPower = Math.pow(7, (-position[2] + 350) / 200);
                attractor.power = attractor.power * 0.5 + wantedPower * 0.5;
            } else {
                attractor.power = attractor.power * 0.5;
            }

            if (attractor.handMesh == null) {
                attractor.handMesh = new HandMesh();
                sketch.scene.add(attractor.handMesh);
            }
            attractor.handMesh.update(sketch.canvas, hand);
            attractor.handMesh!.visible = true;
        });
    });
    return controller;
}
