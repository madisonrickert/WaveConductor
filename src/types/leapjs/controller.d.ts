import EventEmitter from "events";

declare module 'leapjs' {
    /**
     * Controller options interface based on lib/controller.js
     */
    export interface ControllerOptions {
        /** IP or hostname of the computer running the Leap software */
        host?: string;
        /** Port number for the Leap Motion service */
        port?: number;
        /** The frame event type - 'animationFrame' for browser sync, 'deviceFrame' for device rate */
        frameEventName?: 'animationFrame' | 'deviceFrame';
        /** Whether to suppress the animation loop */
        suppressAnimationLoop?: boolean;
        /** Whether to loop while disconnected, maintaining 60FPS even without Leap data */
        loopWhileDisconnected?: boolean;
        /** Whether to use all available plugins */
        useAllPlugins?: boolean;
        /** Whether to check version compatibility */
        checkVersion?: boolean;
        /** Connection type override */
        connectionType?: unknown;
        /** Whether running in Node.js environment */
        inNode?: boolean;
    }

    /**
     * Constructs a Controller object.
     * 
     * When creating a Controller object, you may optionally pass in options
     * to set the host , set the port, or select the frame event type.
     *
     * ```javascript
     * var controller = new Leap.Controller({
     *   host: '127.0.0.1',
     *   port: 6437,
     *   frameEventName: 'animationFrame'
     * });
     * ```
     *
     * @class Controller
     * @memberof Leap
     * @classdesc
     * The Controller class is your main interface to the Leap Motion Controller.
     *
     * Create an instance of this Controller class to access frames of tracking data
     * and configuration information. Frame data can be polled at any time using the
     * [Controller.frame]{@link Leap.Controller#frame}() function. Call frame() or frame(0) to get the most recent
     * frame. Set the history parameter to a positive integer to access previous frames.
     * A controller stores up to 60 frames in its frame history.
     *
     * Polling is an appropriate strategy for applications which already have an
     * intrinsic update loop, such as a game.
     *
     * loopWhileDisconnected defaults to true, and maintains a 60FPS frame rate even when Leap Motion is not streaming
     * data at that rate (such as no hands in frame).  This is important for VR/WebGL apps which rely on rendering for
     * regular visual updates, including from other input devices.  Flipping this to false should be considered an
     * optimization for very specific use-cases.
     */
    export class Controller extends EventEmitter {
        /**
         * Most recent received Frame.
         */
        private latestFrame: Frame;
        
        /**
         * Socket connection.
         */
        connection: BaseConnection;
        
        lastConnectionFrame: Frame;
        lastFrame: Frame;
        lastValidFrame: Frame;
        
        /** Whether running in Node.js environment */
        inNode: boolean;
        
        /** Animation frame request state */
        animationFrameRequested: boolean;
        
        /** Animation frame callback */
        onAnimationFrame: (timestamp: number) => void;
        
        /** Whether animation loop is suppressed */
        suppressAnimationLoop: boolean;
        
        /** Whether to loop while disconnected */
        loopWhileDisconnected: boolean;
        
        /** Frame event name */
        frameEventName: string;
        
        /** Whether to use all plugins */
        useAllPlugins: boolean;
        
        /** Frame history buffer */
        history: CircularBuffer<Frame>;
        
        /** Whether to check version */
        checkVersion: boolean;
        
        /** Connection type constructor */
        connectionType: unknown;
        
        /** Streaming count */
        streamingCount: number;
        
        /** Connected devices */
        devices: Record<string, unknown>;
        
        /** Loaded plugins */
        plugins: Record<string, unknown>;
        
        /** Plugin pipeline steps */
        private _pluginPipelineSteps: Record<string, unknown>;
        
        /** Plugin extended methods */
        private _pluginExtendedMethods: Record<string, unknown>;

        /**
         * Constructs a Controller object.
         * @param opts Controller options
         */
        constructor(opts?: ControllerOptions);

        /**
         * Finds a Hand object by ID.
         * @param frame The Frame object in which the Hand contains
         * @param id The ID of the Hand object
         * @return The Hand object if found, otherwise null
         */
        private static getHandByID(frame: Frame, id: string): Hand | null;
        
        /**
         * Finds a Pointable object by ID.
         * @param frame The Frame object in which the Pointable contains
         * @param id The ID of the Pointable object
         * @return The Pointable object if found, otherwise null
         */
        private static getPointableByID(frame: Frame, id: string): Pointable | null;

        /**
         * Returns a frame of tracking data from the Leap.
         *
         * Use the optional history parameter to specify which frame to retrieve.
         * Call frame() or frame(0) to access the most recent frame; call frame(1) to
         * access the previous frame, and so on. If you use a history value greater
         * than the number of stored frames, then the controller returns an invalid frame.
         *
         * @method frame
         * @memberof Leap.Controller.prototype
         * @param {number} history The age of the frame to return, counting backwards from
         * the most recent frame (0) into the past and up to the maximum age (59).
         * @returns {Frame} The specified frame; or, if no history
         * parameter is specified, the newest frame. If a frame is not available at
         * the specified history position, an invalid Frame is returned.
         */
        frame(history?: number): Frame;

        /**
         * Reports whether this Controller is connected to the Leap Motion Controller.
         *
         * When you first create a Controller object, connected() returns false.
         * After the controller finishes initializing and connects to
         * the Leap, connected() will return true.
         *
         * You can either handle the onConnect event using a event listener
         * or poll the connected() if you need to wait for your
         * application to be connected to the Leap before performing
         * some other operation.
         *
         * @return True, if connected; false otherwise.
         */
        connected(): boolean;

        /**
         * Establish connection to the Leap Motion service.
         * @returns The controller instance for chaining
         */
        connect(): this;

        /**
         * Disconnect from the Leap Motion service.
         * @param allowReconnect Whether to allow automatic reconnection
         * @returns The controller instance for chaining
         */
        disconnect(allowReconnect?: boolean): this;

        /**
         * Reports whether the controller is currently streaming data.
         * @returns True if streaming; false otherwise
         */
        streaming(): boolean;

        /**
         * Enable or disable gesture recognition.
         * @param enabled Whether gestures should be enabled
         */
        enableGestures(enabled: boolean): void;

        /**
         * Set the background state of the connection.
         * @param state Whether the connection is in background mode
         * @returns The controller instance for chaining
         */
        setBackground(state: boolean): this;

        /**
         * Set HMD optimization state.
         * @param state Whether to optimize for head-mounted displays
         * @returns The controller instance for chaining
         */
        setOptimizeHMD(state: boolean): this;

        /**
         * Use a plugin with the controller.
         * @param pluginName Name of the plugin to use
         * @param options Plugin options
         * @returns The controller instance for chaining
         */
        use(pluginName: string, options?: Record<string, unknown>): this;

        /**
         * Stop using a plugin.
         * @param pluginName Name of the plugin to stop using
         * @returns The controller instance for chaining
         */
        stopUsing(pluginName: string): this;

        /**
         * Start the controller loop with a callback function.
         * @param callback Function to call for each frame
         * @returns The controller instance for chaining
         */
        loop(callback: (frame: Frame) => void): this;

        /**
         * Add a processing step to the pipeline.
         * @param step Function to process each frame
         */
        addStep(step: (frame: Frame) => void): void;

        /**
         * Process a frame through the pipeline.
         * @param frame Frame to process
         */
        processFrame(frame: Frame): void;

        /**
         * Process a finished frame.
         * @param frame Completed frame
         */
        processFinishedFrame(frame: Frame): void;

        /**
         * Emit hand-related events.
         * @param frame Frame containing hand data
         */
        emitHandEvents(frame: Frame): void;

        /**
         * Setup frame event handling.
         * @param opts Event options
         */
        setupFrameEvents(opts: Record<string, unknown>): void;

        /**
         * Setup connection event handling.
         */
        setupConnectionEvents(): void;

        /**
         * Check if the software is out of date.
         */
        checkOutOfDate(): void;

        /**
         * Determine if running in browser environment.
         * @returns True if in browser; false otherwise
         */
        inBrowser(): boolean;

        /**
         * Determine if animation loop should be used.
         * @returns True if animation loop should be used
         */
        useAnimationLoop(): boolean;

        /**
         * Determine if running in background page.
         * @returns True if in background page
         */
        inBackgroundPage(): boolean;

        /**
         * Start the animation loop.
         */
        startAnimationLoop(): void;

        /**
         * Use all registered plugins.
         */
        useRegisteredPlugins(): void;

        /**
         * Register a plugin with the Controller class.
         * @param pluginName Name of the plugin
         * @param factory Factory function for creating plugin instances
         */
        static plugin(pluginName: string, factory: (options?: Record<string, unknown>) => unknown): void;

        /**
         * Get list of available plugins.
         * @returns Array of plugin names
         */
        static plugins(): string[];
    }
}
