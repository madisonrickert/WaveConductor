import * as Leap from "leapjs";
import * as THREE from "three";
import { HandMesh } from "./handMesh";
import { mapLeapToThreePosition, wireLeapConnectionEvents } from "./util";
import { LeapConnectionStatus } from "@/common/leapStatus";

export interface LeapHandInfo {
    hand: Leap.Hand;
    index: number;
    canvasPosition: { x: number; y: number };
}

export type HandRenderMode =
    | { type: "in-scene"; scene: THREE.Scene }
    | { type: "overlay" };

export interface LeapHandControllerOptions {
    canvas: HTMLCanvasElement;
    renderer: THREE.WebGLRenderer;
    getConnectionCallback: () => ((status: LeapConnectionStatus) => void) | undefined;
    getProtocolVersionCallback?: () => ((version: number | null) => void) | undefined;
    renderMode: HandRenderMode;
    handMaterial?: THREE.MeshBasicMaterial;
    onFrame: (hands: LeapHandInfo[]) => void;
}

export class LeapHandController {
    public activeHandCount = 0;

    private _handScene?: THREE.Scene;
    private _handCamera?: THREE.OrthographicCamera;
    private _controller: Leap.Controller;
    private _handMeshesGroup = new THREE.Group();
    private _cleanupConnectionEvents: () => void;
    private _options: LeapHandControllerOptions;

    get handScene(): THREE.Scene | undefined { return this._handScene; }
    get handCamera(): THREE.OrthographicCamera | undefined { return this._handCamera; }

    constructor(options: LeapHandControllerOptions) {
        this._options = options;

        if (options.renderMode.type === "in-scene") {
            options.renderMode.scene.add(this._handMeshesGroup);
        } else {
            this._handScene = new THREE.Scene();
            this._handCamera = new THREE.OrthographicCamera(
                0, options.canvas.width,
                0, options.canvas.height,
                1, 1000,
            );
            this._handCamera.position.z = 500;
            this._handScene.add(this._handMeshesGroup);
        }

        this._controller = new Leap.Controller();
        this._controller
            .connect()
            .on("frame", this._handleFrame);

        this._cleanupConnectionEvents = wireLeapConnectionEvents(
            this._controller,
            options.getConnectionCallback,
            options.getProtocolVersionCallback,
        );
    }

    public renderOverlay(): void {
        if (this.activeHandCount === 0 || !this._handScene || !this._handCamera) return;
        const renderer = this._options.renderer;
        const prevAutoClear = renderer.autoClear;
        renderer.autoClear = false;
        renderer.render(this._handScene, this._handCamera);
        renderer.autoClear = prevAutoClear;
    }

    public resize(width: number, height: number): void {
        if (this._handCamera) {
            this._handCamera.right = width;
            this._handCamera.bottom = height;
            this._handCamera.updateProjectionMatrix();
        }
    }

    public dispose(): void {
        this._cleanupConnectionEvents();
        this._controller
            .removeListener("frame", this._handleFrame)
            .disconnect();

        // Remove hand meshes group from its parent scene
        this._handMeshesGroup.removeFromParent();

        // Dispose hand material if one was provided (we own it)
        this._options.handMaterial?.dispose();
    }

    private _getHandMesh(index: number): HandMesh {
        while (this._handMeshesGroup.children.length <= index) {
            const handMesh = new HandMesh(this._options.handMaterial);
            handMesh.visible = false;
            this._handMeshesGroup.add(handMesh);
        }
        return this._handMeshesGroup.children[index] as HandMesh;
    }

    private _handleFrame = (frame: Leap.Frame): void => {
        const validHands = frame.hands.filter((h) => h.valid);
        this.activeHandCount = validHands.length;

        const handInfos: LeapHandInfo[] = validHands.map((hand, index) => {
            const canvasPosition = mapLeapToThreePosition(this._options.canvas, hand.palmPosition);
            const handMesh = this._getHandMesh(index);
            handMesh.update(this._options.canvas, hand);
            handMesh.visible = true;
            return { hand, index, canvasPosition };
        });

        for (let i = validHands.length; i < this._handMeshesGroup.children.length; i++) {
            this._handMeshesGroup.children[i].visible = false;
        }

        this._options.onFrame(handInfos);
    };
}
