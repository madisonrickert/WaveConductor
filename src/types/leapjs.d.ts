// Type definitions for leapjs 1.1.1

// Import all the individual type files to register their module augmentations
import './leapjs/controller';
import './leapjs/frame';
import './leapjs/hand';
import './leapjs/pointable';
import './leapjs/finger';
import './leapjs/bone';
import './leapjs/interaction-box';
import './leapjs/circular-buffer';
import './leapjs/connection';

declare module 'leapjs' {
    // Additional exports that aren't in the individual files
    export const glMatrix: typeof import("gl-matrix");
    export const mat3: typeof import("gl-matrix").mat3;
    export const vec3: typeof import("gl-matrix").vec3;
    
    export let loopController: Controller | undefined;
    
    export const version: {
        full: string;
        major: number;
        minor: number;
        dot: number;
    };

    /**
     * The Leap.loop() function passes a frame of Leap data to your
     * callback function and then calls window.requestAnimationFrame() after
     * executing your callback function.
     *
     * Leap.loop() sets up the Leap controller and WebSocket connection for you.
     * You do not need to create your own controller when using this method.
     *
     * Your callback function is called on an interval determined by the client
     * browser. Typically, this is on an interval of 60 frames/second. The most
     * recent frame of Leap data is passed to your callback function. If the Leap
     * is producing frames at a slower rate than the browser frame rate, the same
     * frame of Leap data can be passed to your function in successive animation
     * updates.
     *
     * As an alternative, you can create your own Controller object and use a
     * {@link Controller#onFrame onFrame} callback to process the data at
     * the frame rate of the Leap device. See {@link Controller} for an
     * example.
     *
     * @method Leap.loop
     * @param {function} callback A function called when the browser is ready to
     * draw to the screen. The most recent {@link Frame} object is passed to
     * your callback function.
     *
     * ```javascript
     *    Leap.loop( function( frame ) {
     *        // ... your code here
     *    })
     * ```
     */
    export function loop(callback: (frame: Frame) => void): Controller;
    export function loop(opts: Record<string, unknown>, callback: (frame: Frame) => void): Controller;

    /**
     * Convenience method for Leap.Controller.plugin
     */
    export function plugin(name: string, options?: Record<string, unknown>): void;
}