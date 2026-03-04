import React from "react";
import * as THREE from "three";
import { HandData } from "./components/HandOverlay";

export const UI_EVENTS = {
    click: true,
    contextmenu: true,
    dblclick: true,
    mousedown: true,
    mouseup: true,
    mousemove: true,
    touchstart: true,
    touchmove: true,
    touchend: true,
    keyup: true,
    keydown: true,
    keypress: true,
    wheel: true,
} as const;

export type UIEventName = keyof typeof UI_EVENTS;

type UIEventMap = Pick<GlobalEventHandlersEventMap, UIEventName>;

export type UIEventHandler<E extends UIEventName = UIEventName> = (event: UIEventMap[E]) => void;

export type UIEventReceiver = Partial<{ [E in UIEventName]: UIEventHandler<E> }>;

export abstract class Sketch {
    static id?: string;

    public elements?: React.JSX.Element[];
    public events?: UIEventReceiver;
    constructor(public renderer: THREE.WebGLRenderer, public audioContext: SketchAudioContext) {}

    /**
     * height / width
     */
    get aspectRatio() {
        return this.renderer.domElement.height / this.renderer.domElement.width;
    }

    get resolution() {
        return new THREE.Vector2(this.renderer.domElement.width, this.renderer.domElement.height);
    }

    get canvas() {
        return this.renderer.domElement;
    }

    /**
     * Converts client (viewport) coordinates to canvas-relative coordinates
     * by subtracting the canvas element's bounding rect offset.
     */
    protected getRelativeCoordinates(clientX: number, clientY: number) {
        const rect = this.canvas.getBoundingClientRect();
        return {
            x: clientX - rect.left,
            y: clientY - rect.top,
        };
    }

    /**
     * Called in componentDidMount of the Sketch component.
     */
    abstract init(): void;

    /**
     * Called once per frame to animate the sketch.
     * @param _millisElapsed Time elapsed since the last frame in milliseconds.
     */
    abstract animate(_millisElapsed: number): void;

    render?(): React.ReactNode;

    resize?(width: number, height: number): void;

    destroy?(): void;

    /**
     * Callback to update the screen saver state.
     * This is set by the parent component to control the visibility of the screen saver.
     */
    public updateScreenSaverCallback?: (shouldShow: boolean) => void;

    /**
     * Callback to handle hand data updates.
     * This is set by the parent component to receive hand data updates.
     */
    public updateHandDataCallback?: (handData: HandData[]) => void;
}

export interface SketchConstructor {
    new (renderer: THREE.WebGLRenderer, audioContext: SketchAudioContext): Sketch;

    id?: string;
}

export interface SketchAudioContext extends AudioContext {
    gain: GainNode;
}
