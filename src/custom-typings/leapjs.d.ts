// much of this is taken from https://github.com/logotype/LeapMotionTS/blob/master/build/leapmotionts-2.2.4.d.ts
declare module 'leapjs' {
    import EventEmitter from "events";

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
    export function loop(cb: (frame: Frame) => void): Controller;

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
     *
     *
     */
    export class Controller extends EventEmitter {
        /**
         * Most recent received Frame.
         */
        private latestFrame;
        /**
         * Socket connection.
         */
        connection: WebSocket;
        lastConnectionFrame: Frame;
        lastFrame: Frame;
        lastValidFrame: Frame;
        /**
         * Constructs a Controller object.
         * @param host IP or hostname of the computer running the Leap software.
         * (currently only supported for socket connections).
         *
         */
        constructor(opts?: {
            host?: string;
            port?: number;
            frameEventName?: 'animationFrame' | 'deviceFrame';
            suppressAnimationLoop?: boolean;
            loopWhileDisconnected?: boolean;
            useAllPlugins?: boolean;
            checkVersion?: boolean;
            connectionType?: any;
            inNode?: boolean;
        });

        /**
         * Finds a Hand object by ID.
         *
         * @param frame The Frame object in which the Hand contains
         * @param id The ID of the Hand object
         * @return The Hand object if found, otherwise null
         *
         */
        private static getHandByID(frame, id);
        /**
         * Finds a Pointable object by ID.
         *
         * @param frame The Frame object in which the Pointable contains
         * @param id The ID of the Pointable object
         * @return The Pointable object if found, otherwise null
         *
         */
        private static getPointableByID(frame, id);
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
         **/
        frame(history?: number): Frame;
        /**
         * Reports whether this Controller is connected to the Leap Motion Controller.
         *
         * <p>When you first create a Controller object, <code>connected()</code> returns false.
         * After the controller finishes initializing and connects to
         * the Leap, <code>connected()</code> will return true.</p>
         *
         * <p>You can either handle the onConnect event using a event listener
         * or poll the <code>connected()</code> if you need to wait for your
         * application to be connected to the Leap before performing
         * some other operation.</p>
         *
         * @return True, if connected; false otherwise.
         *
         */
        connected(): boolean;
        connect(): this;
        disconnect(allowReconnect?: boolean): this;
        streaming(): boolean;
        enableGestures(enabled: boolean): void;
        setBackground(state: boolean): this;
        setOptimizeHMD(state: boolean): this;
        use(pluginName: string, options?: object): this;
        stopUsing(pluginName: string): this;
        loop(callback: (frame: Frame) => void): this;
        addStep(step: (frame: Frame) => void): void;
        processFrame(frame: Frame): void;
        processFinishedFrame(frame: Frame): void;
        emitHandEvents(frame: Frame): void;
        setupFrameEvents(opts: object): void;
        setupConnectionEvents(): void;
        checkOutOfDate(): void;
        static plugin(pluginName: string, factory: (options?: object) => object): void;
        static plugins(): string[];
    }

    /**
     * The InteractionBox class represents a box-shaped region completely within
     * the field of view of the Leap Motion controller.
     *
     * <p>The interaction box is an axis-aligned rectangular prism and provides
     * normalized coordinates for hands, fingers, and tools within this box.
     * The InteractionBox class can make it easier to map positions in the
     * Leap Motion coordinate system to 2D or 3D coordinate systems used
     * for application drawing.</p>
     *
     * <p>The InteractionBox region is defined by a center and dimensions along the x, y, and z axes.</p>
     *
     * @author logotype
     *
     */
    export class InteractionBox {
        /**
         * Indicates whether this is a valid InteractionBox object.
         */
        valid: boolean;
        /**
         * The center of the InteractionBox in device coordinates (millimeters).
         * This point is equidistant from all sides of the box.
         */
        center: number[];
        /**
         * The size vector [width, height, depth] of the InteractionBox in millimeters.
         */
        size: number[];
        /**
         * The width of the InteractionBox in millimeters, measured along the x-axis.
         */
        width: number;
        /**
         * The height of the InteractionBox in millimeters, measured along the y-axis.
         */
        height: number;
        /**
         * The depth of the InteractionBox in millimeters, measured along the z-axis.
         */
        depth: number;
        constructor(data?: any);
        /**
         * Converts a position defined by normalized InteractionBox coordinates
         * into device coordinates in millimeters.
         * @param normalizedPosition The input position in InteractionBox coordinates.
         * @returns The corresponding denormalized position in device coordinates.
         */
        denormalizePoint(normalizedPosition: number[]): number[];
        /**
         * Normalizes the coordinates of a point using the interaction box.
         * @param position The input position in device coordinates.
         * @param clamp Whether or not to limit the output value to the range [0,1] when the input position is outside the InteractionBox. Defaults to true.
         * @returns The normalized position.
         */
        normalizePoint(position: number[], clamp?: boolean): number[];
        /**
         * Writes a brief, human readable description of the InteractionBox object.
         * @returns A description of the InteractionBox as a string.
         */
        toString(): string;
        /**
         * An invalid InteractionBox object.
         */
        static Invalid: InteractionBox;
    }
    export class Pointable {
        /**
         * Indicates whether this is a valid Pointable object.
         */
        valid: boolean;
        /**
         * A unique ID assigned to this Pointable object.
         */
        id: string;
        /**
         * The ID of the hand this pointable is attached to.
         */
        handId: string;
        /**
         * The estimated length of the finger or tool in millimeters.
         */
        length: number;
        /**
         * Whether or not the Pointable is believed to be a tool.
         */
        tool: boolean;
        /**
         * The estimated width of the tool in millimeters.
         */
        width: number;
        /**
         * The direction in which this finger or tool is pointing.
         */
        direction: number[];
        /**
         * The tip position in millimeters from the Leap origin (stabilized).
         */
        stabilizedTipPosition: number[];
        /**
         * The tip position in millimeters from the Leap origin.
         */
        tipPosition: number[];
        /**
         * The rate of change of the tip position in millimeters/second.
         */
        tipVelocity: number[];
        /**
         * The current touch zone of this Pointable object.
         */
        touchZone: string;
        /**
         * A value proportional to the distance between this Pointable object and the adaptive touch plane.
         */
        touchDistance: number;
        /**
         * How long the pointable has been visible in seconds.
         */
        timeVisible: number;
        /**
         * Returns the hand which the pointable is attached to.
         */
        hand(): Hand;
        /**
         * A string containing a brief, human readable description of the Pointable object.
         */
        toString(): string;
        /**
         * An invalid Pointable object.
         */
        static Invalid: Pointable;
        constructor(data?: any);
    }

    export class Finger extends Pointable {
        /**
         * The position of the distal interphalangeal joint of the finger.
         */
        dipPosition: number[];
        /**
         * The position of the proximal interphalangeal joint of the finger.
         */
        pipPosition: number[];
        /**
         * The position of the metacarpopophalangeal joint, or knuckle, of the finger.
         */
        mcpPosition: number[];
        /**
         * The position of the Carpometacarpal joint.
         */
        carpPosition: number[];
        /**
         * Whether or not this finger is in an extended posture.
         */
        extended: boolean;
        /**
         * An integer code for the name of this finger.
         * 0 -- thumb, 1 -- index, 2 -- middle, 3 -- ring, 4 -- pinky
         */
        type: number;
        /**
         * The joint positions of this finger as an array in the order base to tip.
         */
        positions: number[][];
        /**
         * The metacarpal bone of the finger.
         */
        metacarpal: Bone;
        /**
         * The proximal bone of the finger.
         */
        proximal: Bone;
        /**
         * The medial bone of the finger.
         */
        medial: Bone;
        /**
         * The distal bone of the finger.
         */
        distal: Bone;
        /**
         * All bones of the finger.
         */
        bones: Bone[];
        /**
         * A string containing a brief, human readable description of the Finger object.
         */
        toString(): string;
        /**
         * An invalid Finger object.
         */
        static Invalid: Finger;
        constructor(data?: any);
    }

    export class Bone {
        finger: Finger;
        /**
         * An integer code for the name of this bone.
         *
         * * 0 -- metacarpal
         * * 1 -- proximal
         * * 2 -- medial
         * * 3 -- distal
         * * 4 -- arm
         */
        type: number;
        /**
         * The position of the previous, or base joint of the bone closer to the wrist.
         */
        prevJoint: number[];
        /**
         * The position of the next joint, or the end of the bone closer to the finger tip.
         */
        nextJoint: number[];
        /**
         * The estimated width of the pointable in millimeters.
         *
         * The reported width is the average width of the visible portion of the
         * pointable from the hand to the tip. If the width isn't known,
         * then a value of 0 is returned.
         *
         * Bone objects representing fingers do not have a width property.
         */
        width: number;
        length: number;
        /**
         *
         * These fully-specify the orientation of the bone.
         * See examples/threejs-bones.html for more info
         * Three vec3s:
         *  x (red): The rotation axis of the finger, pointing outwards.  (In general, away from the thumb )
         *  y (green): The "up" vector, orienting the top of the finger
         *  z (blue): The roll axis of the bone.
         *
         *  Most up vectors will be pointing the same direction, except for the thumb, which is more rightwards.
         *
         *  The thumb has one fewer bones than the fingers, but there are the same number of joints & joint-bases provided
         *  the first two appear in the same position, but only the second (proximal) rotates.
         *
         *  Normalized.
         */
        basis: number[][];

        left(): boolean;
        /**
         * The Affine transformation matrix describing the orientation of the bone, in global Leap-space.
         * It contains a 3x3 rotation matrix (in the "top left"), and center coordinates in the fourth column.
         *
         * Unlike the basis, the right and left hands have the same coordinate system.
         *
         */
        matrix(): number[];
        /**
         * Helper method to linearly interpolate between the two ends of the bone.
         *
         * when t = 0, the position of prevJoint will be returned
         * when t = 1, the position of nextJoint will be returned
         */
        lerp(out: number[], t: number): void;
        /**
         *
         * The center position of the bone
         * Returns a vec3 array.
         *
         */
        center(): number[];
        /**
         * The negative of the z-basis
         */
        direction(): number[];
    }

    export class Hand {
        /**
         * Returns an invalid Hand object.
         *
         * You can use the instance returned by this in comparisons
         * testing whether a given Hand instance is valid or invalid.
         * (You can also use the <code>Hand.isValid()</code> function.)
         *
         * @return The invalid Hand instance.
         */
        static Invalid: Hand;

        /**
         * A unique ID assigned to this Hand object, whose value remains the same
         * across consecutive frames while the tracked hand remains visible. If
         * tracking is lost (for example, when a hand is occluded by another hand
         * or when it is withdrawn from or reaches the edge of the Leap field of view),
         * the Leap may assign a new ID when it detects the hand in a future frame.
         *
         * Use the ID value with the {@link Frame.hand}() function to find this
         * Hand object in future frames.
         */
        id: string;

        /**
         * The center position of the palm in millimeters from the Leap origin.
         */
        palmPosition: number[];

        /**
         * The direction from the palm position toward the fingers.
         *
         * The direction is expressed as a unit vector pointing in the same
         * direction as the directed line from the palm position to the fingers.
         */
        direction: number[];

        /**
         * The rate of change of the palm position in millimeters/second.
         */
        palmVelocity: number[];

        /**
         * The normal vector to the palm. If your hand is flat, this vector will
         * point downward, or "out" of the front surface of your palm.
         *
         * The direction is expressed as a unit vector pointing in the same
         * direction as the palm normal (that is, a vector orthogonal to the palm).
         */
        palmNormal: number[];

        /**
         * The center of a sphere fit to the curvature of this hand.
         *
         * This sphere is placed roughly as if the hand were holding a ball.
         */
        sphereCenter: number[];

        /**
         * The radius of a sphere fit to the curvature of this hand, in millimeters.
         *
         * This sphere is placed roughly as if the hand were holding a ball. Thus the
         * size of the sphere decreases as the fingers are curled into a fist.
         */
        sphereRadius: number;

        /**
         * Reports whether this is a valid Hand object.
         */
        valid: boolean;

        /**
         * The list of Pointable objects (fingers) detected in this frame
         * that are associated with this hand, given in arbitrary order. The list
         * can be empty if no fingers or tools associated with this hand are detected.
         *
         * Use the {@link Pointable} tool property to determine
         * whether or not an item in the list represents a tool or finger.
         * You can also get only the fingers using the Hand.fingers[] list.
         */
        pointables: Pointable[];

        /**
         * The list of fingers detected in this frame that are attached to
         * this hand, given in arbitrary order.
         *
         * The list can be empty if no fingers attached to this hand are detected.
         */
        fingers: Finger[];
        /**
         * Shortcut to the thumb finger, if present.
         */
        thumb?: Finger;
        /**
         * Shortcut to the index finger, if present.
         */
        indexFinger?: Finger;
        /**
         * Shortcut to the middle finger, if present.
         */
        middleFinger?: Finger;
        /**
         * Shortcut to the ring finger, if present.
         */
        ringFinger?: Finger;
        /**
         * Shortcut to the pinky finger, if present.
         */
        pinky?: Finger;

        /**
         * The arm bone associated with this hand, or null if not present.
         */
        arm: Bone | null;

        /**
         * Time the hand has been visible in seconds.
         */
        timeVisible: number;

        /**
         * The palm position with stabilization.
         */
        stabilizedPalmPosition: number[];

        /**
         * Reports whether this is a left or a right hand.
         */
        type: string;

        /**
         * The grab strength of the hand.
         */
        grabStrength: number;

        /**
         * The pinch strength of the hand.
         */
        pinchStrength: number;

        /**
         * The confidence level of the hand tracking.
         */
        confidence: number;

        /**
         * Constructs a Hand object.
         *
         * An uninitialized hand is considered invalid.
         * Get valid Hand objects from a Frame object.
         */
        constructor(data?: any);

        /**
         * The finger with the specified ID attached to this hand.
         *
         * Use this function to retrieve a Pointable object representing a finger
         * attached to this hand using an ID value obtained from a previous frame.
         * This function always returns a Pointable object, but if no finger
         * with the specified ID is present, an invalid Pointable object is returned.
         *
         * @param id The ID value of a finger from a previous frame.
         * @returns The Finger object with the matching ID if one exists for this hand in this frame; otherwise, an invalid Finger object is returned.
         */
        finger(id: string): Pointable;

        /**
         * The angle of rotation around the rotation axis derived from the change in
         * orientation of this hand, and any associated fingers, between the
         * current frame and the specified frame.
         *
         * The returned angle is expressed in radians measured clockwise around the
         * rotation axis (using the right-hand rule) between the start and end frames.
         * The value is always between 0 and pi radians (0 and 180 degrees).
         *
         * If a corresponding Hand object is not found in sinceFrame, or if either
         * this frame or sinceFrame are invalid Frame objects, then the angle of rotation is zero.
         *
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @param axis The axis to measure rotation around.
         * @returns A positive value representing the heuristically determined
         * rotational change of the hand between the current frame and that specified in the sinceFrame parameter.
         */
        rotationAngle(sinceFrame: Frame, axis?: number[]): number;

        /**
         * The axis of rotation derived from the change in orientation of this hand, and
         * any associated fingers, between the current frame and the specified frame.
         *
         * The returned direction vector is normalized.
         *
         * If a corresponding Hand object is not found in sinceFrame, or if either
         * this frame or sinceFrame are invalid Frame objects, then this method returns a zero vector.
         *
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @returns A normalized direction Vector representing the axis of the heuristically determined
         * rotational change of the hand between the current frame and that specified in the sinceFrame parameter.
         */
        rotationAxis(sinceFrame: Frame): number[];

        /**
         * The transform matrix expressing the rotation derived from the change in
         * orientation of this hand, and any associated fingers, between
         * the current frame and the specified frame.
         *
         * If a corresponding Hand object is not found in sinceFrame, or if either
         * this frame or sinceFrame are invalid Frame objects, then this method returns
         * an identity matrix.
         *
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @returns A transformation Matrix containing the heuristically determined
         * rotational change of the hand between the current frame and that specified in the sinceFrame parameter.
         */
        rotationMatrix(sinceFrame: Frame): number[];

        /**
         * The scale factor derived from the hand's motion between the current frame and the specified frame.
         *
         * The scale factor is always positive. A value of 1.0 indicates no scaling took place.
         * Values between 0.0 and 1.0 indicate contraction and values greater than 1.0 indicate expansion.
         *
         * The Leap derives scaling from the relative inward or outward motion of a hand
         * and its associated fingers (independent of translation and rotation).
         *
         * If a corresponding Hand object is not found in sinceFrame, or if either this frame or sinceFrame
         * are invalid Frame objects, then this method returns 1.0.
         *
         * @param sinceFrame The starting frame for computing the relative scaling.
         * @returns A positive value representing the heuristically determined
         * scaling change ratio of the hand between the current frame and that specified in the sinceFrame parameter.
         */
        scaleFactor(sinceFrame: Frame): number;

        /**
         * The change of position of this hand between the current frame and the specified frame
         *
         * The returned translation vector provides the magnitude and direction of the
         * movement in millimeters.
         *
         * If a corresponding Hand object is not found in sinceFrame, or if either this frame or
         * sinceFrame are invalid Frame objects, then this method returns a zero vector.
         *
         * @param sinceFrame The starting frame for computing the relative translation.
         * @returns A Vector representing the heuristically determined change in hand
         * position between the current frame and that specified in the sinceFrame parameter.
         */
        translation(sinceFrame: Frame): number[];

        /**
         * The pitch angle in radians.
         *
         * Pitch is the angle between the negative z-axis and the projection of
         * the vector onto the y-z plane. In other words, pitch represents rotation
         * around the x-axis.
         * If the vector points upward, the returned angle is between 0 and pi radians
         * (180 degrees); if it points downward, the angle is between 0 and -pi radians.
         *
         * @returns The angle of this vector above or below the horizon (x-z plane).
         */
        pitch(): number;

        /**
         * The yaw angle in radians.
         *
         * Yaw is the angle between the negative z-axis and the projection of
         * the vector onto the x-z plane. In other words, yaw represents rotation
         * around the y-axis. If the vector points to the right of the negative z-axis,
         * then the returned angle is between 0 and pi radians (180 degrees);
         * if it points to the left, the angle is between 0 and -pi radians.
         *
         * @returns The angle of this vector to the right or left of the y-axis.
         */
        yaw(): number;

        /**
         * The roll angle in radians.
         *
         * Roll is the angle between the y-axis and the projection of
         * the vector onto the x-y plane. In other words, roll represents rotation
         * around the z-axis. If the vector points to the left of the y-axis,
         * then the returned angle is between 0 and pi radians (180 degrees);
         * if it points to the right, the angle is between 0 and -pi radians.
         *
         * @returns The angle of this vector to the right or left of the y-axis.
         */
        roll(): number;

        /**
         * A string containing a brief, human readable description of the Hand object.
         * @returns A description of the Hand as a string.
         */
        toString(): string;
    }
    /**
     * The Frame class represents a set of hand and finger tracking
     * data detected in a single frame.
     *
     * <p>The Leap detects hands, fingers and tools within the tracking area,
     * reporting their positions, orientations and motions in frames at
     * the Leap frame rate.</p>
     *
     * <p>Access Frame objects through a listener of a Leap Controller.
     * Add a listener to receive events when a new Frame is available.</p>
     *
     * @author logotype
     *
     */
    export class Frame {
        /**
         * Reports whether this Frame instance is valid.
         */
        valid: boolean;
        /**
         * A unique ID for this Frame. Consecutive frames processed by the Leap have consecutive increasing values.
         */
        id: string;
        /**
         * The frame capture time in microseconds elapsed since the Leap started.
         */
        timestamp: number;
        /**
         * The list of Hand objects detected in this frame, given in arbitrary order.
         * The list can be empty if no hands are detected.
         */
        hands: Hand[];
        /**
         * The list of Pointable objects (fingers) detected in this frame, given in arbitrary order.
         * The list can be empty if no fingers are detected.
         */
        pointables: Pointable[];
        /**
         * The list of Finger objects detected in this frame, given in arbitrary order.
         * The list can be empty if no fingers are detected.
         */
        fingers: Finger[];
        /**
         * The InteractionBox associated with the current frame.
         */
        interactionBox?: InteractionBox;
        /**
         * Map of hand IDs to Hand objects.
         */
        handsMap: { [id: string]: Hand };
        /**
         * Map of pointable IDs to Pointable objects.
         */
        pointablesMap: { [id: string]: Pointable };
        /**
         * Raw frame data.
         */
        data: any;
        /**
         * Frame type (used by event emitting).
         */
        type: string;
        /**
         * The current frame rate.
         */
        currentFrameRate: number;
        /**
         * Returns the Pointable object with the specified ID in this frame.
         * @param id The ID value of a Pointable object from a previous frame.
         * @returns The Pointable object with the matching ID if one exists in this frame; otherwise, an invalid Pointable object is returned.
         */
        pointable(id: string): Pointable;
        /**
         * Returns the Finger object with the specified ID in this frame.
         * @param id The ID value of a finger from a previous frame.
         * @returns The finger with the matching ID if one exists in this frame; otherwise, an invalid Pointable object is returned.
         */
        finger(id: string): Pointable;
        /**
         * Returns the Hand object with the specified ID in this frame.
         * @param id The ID value of a Hand object from a previous frame.
         * @returns The Hand object with the matching ID if one exists in this frame; otherwise, an invalid Hand object is returned.
         */
        hand(id: string): Hand;
        /**
         * The angle of rotation around the rotation axis derived from the overall rotational motion between the current frame and the specified frame.
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @param axis The axis to measure rotation around.
         * @returns A positive value containing the heuristically determined rotational change between the current frame and that specified in the sinceFrame parameter.
         */
        rotationAngle(sinceFrame: Frame, axis?: number[]): number;
        /**
         * The axis of rotation derived from the overall rotational motion between the current frame and the specified frame.
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @returns A normalized direction vector representing the axis of the heuristically determined rotational change between the current frame and that specified in the sinceFrame parameter.
         */
        rotationAxis(sinceFrame: Frame): number[];
        /**
         * The transform matrix expressing the rotation derived from the overall rotational motion between the current frame and the specified frame.
         * @param sinceFrame The starting frame for computing the relative rotation.
         * @returns A transformation matrix containing the heuristically determined rotational change between the current frame and that specified in the sinceFrame parameter.
         */
        rotationMatrix(sinceFrame: Frame): number[];
        /**
         * The scale factor derived from the overall motion between the current frame and the specified frame.
         * @param sinceFrame The starting frame for computing the relative scaling.
         * @returns A positive value representing the heuristically determined scaling change ratio between the current frame and that specified in the sinceFrame parameter.
         */
        scaleFactor(sinceFrame: Frame): number;
        /**
         * The change of position derived from the overall linear motion between the current frame and the specified frame.
         * @param sinceFrame The starting frame for computing the relative translation.
         * @returns A vector representing the heuristically determined change in position of all objects between the current frame and that specified in the sinceFrame parameter.
         */
        translation(sinceFrame: Frame): number[];
        /**
         * A string containing a brief, human readable description of the Frame object.
         * @returns A brief description of this frame.
         */
        toString(): string;
        /**
         * Returns a JSON-formatted string containing the hands, pointables in this frame.
         * @returns A JSON-formatted string.
         */
        dump(): string;
        /**
         * An invalid Frame object.
         */
        static Invalid: Frame;
    }
    export class CircularBuffer<T> {
        constructor(size: number);
        get(index: number): T;
        push(item: T): void;
    }
    /**
    * Convenience method for Leap.Controller.plugin
    */
    export const plugin: (name: string, options: any) => void;
    export const loopController: undefined | Controller;
    export const version: {
        full: string;
        major: number;
        minor: number;
        dot: number;
    };
}