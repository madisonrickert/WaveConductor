import type React from "react";
import * as THREE from "three";
import { SettingsDefs } from "@/settings/types";
import { LeapConnectionStatus } from "@/leap/leapStatus";
import { LeapHandController, LeapHandControllerOptions } from "@/leap/LeapHandController";

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

/**
 * Abstract base class for all sketches in the gallery.
 *
 * ## Lifecycle
 *
 * A sketch goes through these phases, managed by {@link SketchView} and its hooks:
 *
 * 1. **Construction** — `new SketchClass(renderer, audioContext)`.
 *    The renderer and shared audio context are provided by {@link useSketchInstance}.
 *
 * 2. **Initialization** — {@link init} is called once after mount.
 *    Set up the scene, create audio nodes, spawn particles, and call
 *    {@link createLeapController} to connect Leap Motion input.
 *
 * 3. **Animation loop** — {@link animate} is called once per frame by
 *    `requestAnimationFrame`. The base implementation handles the idle cycle:
 *
 *    ```
 *    Each frame:
 *      → If Leap hands are active, reset the idle timer (markInteraction)
 *      → If the sketch is NOT idle, call step()
 *      → Update idle state and screensaver visibility
 *    ```
 *
 *    Most sketches only implement {@link step}. Sketches that need per-frame
 *    work even while idle (e.g. LineSketch's attractor decay) can override
 *    `animate()` to add that work, then call `super.animate()`.
 *
 * 4. **Resize** — {@link resize} is called when the container changes size.
 *    Update camera projections and shader uniforms here.
 *
 * 5. **Destruction** — {@link destroy} is called on unmount or when a
 *    `requiresRestart` setting changes. Dispose audio nodes, Three.js
 *    resources, and call `super.destroy()` to clean up the Leap controller.
 *
 * ## Idle system
 *
 * After {@link idleTimeoutSeconds} of no interaction (mouse, touch, or Leap),
 * and if {@link isReadyToSleep} returns true, the sketch enters idle mode:
 * {@link step} stops being called. The screensaver overlay appears after
 * {@link screenSaverTimeoutSeconds}. Call {@link markInteraction} from event
 * handlers to reset both timers.
 *
 * ## Input
 *
 * Mouse/touch events are wired from the {@link events} property by
 * {@link SketchRenderer}. Leap Motion input arrives via the `onFrame`
 * callback passed to {@link createLeapController}.
 */
export abstract class BaseSketch {
    static id?: string;

    public events?: UIEventReceiver;
    constructor(public renderer: THREE.WebGLRenderer, public audioContext: SketchAudioContext) {}

    /** Canvas height / width. */
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

    /** Leap Motion controller, created via {@link createLeapController}. */
    protected leapHands?: LeapHandController;

    /** Whether a Leap Motion controller is active (for gating frame workarounds). */
    public get hasLeapController(): boolean {
        return this.leapHands !== undefined;
    }

    /** Called once when the sketch mounts (via useSketchLifecycle). */
    abstract init(): void;

    /**
     * Called once per frame. Checks Leap interaction, gates {@link step} on
     * `!isIdle`, and updates idle state.
     *
     * Subclasses may override to add per-frame work that runs regardless of
     * idle state (e.g. attractor decay), but must call `super.animate()`.
     */
    public animate(_millisElapsed: number): void {
        const currentTimeMs = performance.now();
        if (this.leapHands && this.leapHands.activeHandCount > 0) {
            this.markInteraction(currentTimeMs);
        }
        if (!this.isIdle) {
            this.step(currentTimeMs);
        }
        this.updateIdleState(currentTimeMs);
    }

    /**
     * Called every frame when the sketch is not idle. This is where the sketch's
     * core simulation, audio feedback, and rendering should happen.
     */
    protected abstract step(currentTimeMs: number): void;

    /** Optional React overlay rendered on top of the sketch canvas. */
    render?(): React.ReactNode;

    resize?(width: number, height: number): void;

    /**
     * Cleans up resources when the sketch unmounts. Subclasses should call
     * `super.destroy()` to dispose the Leap controller.
     */
    destroy(): void {
        this.leapHands?.dispose();
    }

    /**
     * Creates a {@link LeapHandController} with the sketch's canvas, renderer,
     * and status callbacks pre-filled. Subclasses provide only the sketch-specific
     * options (renderMode, onFrame, and optionally handMaterial).
     */
    protected createLeapController(
        options: Omit<LeapHandControllerOptions, "canvas" | "renderer" | "getConnectionCallback" | "getProtocolVersionCallback">,
    ): LeapHandController {
        return new LeapHandController({
            canvas: this.canvas,
            renderer: this.renderer,
            getConnectionCallback: () => this.updateLeapConnectionCallback,
            getProtocolVersionCallback: () => this.updateLeapProtocolVersionCallback,
            ...options,
        });
    }

    // --- Idle / Screensaver Tracking ---

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

    /** Evaluates idle timeout and notifies the screensaver overlay. Called at the end of each frame by {@link animate}. */
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
     * Callback to report Leap Motion connection status changes.
     * This is set by the parent component to receive connection status updates.
     */
    public updateLeapConnectionCallback?: (status: LeapConnectionStatus) => void;

    /**
     * Callback to report the negotiated protocol version.
     * Set by the parent component to receive protocol version after connection handshake.
     */
    public updateLeapProtocolVersionCallback?: (version: number | null) => void;
}

export interface SketchConstructor {
    new (renderer: THREE.WebGLRenderer, audioContext: SketchAudioContext): BaseSketch;

    id?: string;
    settings?: SettingsDefs;
    preserveDrawingBuffer?: boolean;
}

export interface SketchAudioContext extends AudioContext {
    gain: GainNode;
}
