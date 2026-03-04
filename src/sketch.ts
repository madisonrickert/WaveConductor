import type React from "react";
import * as THREE from "three";
import { HandData } from "./components/HandOverlay";
import { SettingsDefs } from "./common/sketchSettings";

export type UIEventName =
    | "click"
    | "contextmenu"
    | "dblclick"
    | "mousedown"
    | "mouseup"
    | "mousemove"
    | "touchstart"
    | "touchmove"
    | "touchend"
    | "keyup"
    | "keydown"
    | "keypress"
    | "wheel";

type UIEventMap = Pick<GlobalEventHandlersEventMap, UIEventName>;

export type UIEventHandler<E extends UIEventName = UIEventName> = (event: UIEventMap[E]) => void;

export type UIEventReceiver = Partial<{ [E in UIEventName]: UIEventHandler<E> }>;

export abstract class Sketch {
    static id?: string;

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

    // --- Idle / Screensaver Tracking ---
    // Opt-in: subclasses must call `updateIdleState()` in their `animate()` method
    // and `markInteraction()` in their event handlers to activate this behavior.

    protected lastInteractionTimestampMs: number = performance.now();
    protected isIdle: boolean = false;

    /** Seconds of inactivity before the screensaver overlay appears. */
    protected screenSaverTimeoutSeconds: number = 30;
    /** Seconds of inactivity (plus `isReadyToSleep()`) before the sketch stops simulating. */
    protected idleTimeoutSeconds: number = 30;

    /** Call from event handlers and input sources to reset idle/screensaver timers. */
    protected markInteraction(timestampMs: number = performance.now()) {
        this.lastInteractionTimestampMs = timestampMs;
        this.isIdle = false;
    }

    /**
     * Call once per frame (in `animate`) to update `isIdle` and the screensaver overlay.
     */
    protected updateIdleState(currentTimeMs: number) {
        const secondsSinceInteraction = (currentTimeMs - this.lastInteractionTimestampMs) / 1000;
        this.isIdle = secondsSinceInteraction >= this.idleTimeoutSeconds && this.isReadyToSleep();

        if (this.updateScreenSaverCallback) {
            this.updateScreenSaverCallback(secondsSinceInteraction >= this.screenSaverTimeoutSeconds);
        }
    }

    /** Override to add sketch-specific conditions for entering sleep (e.g. no active attractors). */
    protected isReadyToSleep(): boolean {
        return true;
    }

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
    settings?: SettingsDefs;
}

export interface SketchAudioContext extends AudioContext {
    gain: GainNode;
}
