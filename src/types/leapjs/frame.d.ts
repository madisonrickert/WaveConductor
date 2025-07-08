declare module 'leapjs' {
    /**
     * The Frame class represents a set of hand and finger tracking
     * data detected in a single frame.
     * Based on lib/frame.js
     *
     * The Leap detects hands, fingers and tools within the tracking area,
     * reporting their positions, orientations and motions in frames at
     * the Leap frame rate.
     *
     * Access Frame objects through a listener of a Leap Controller.
     * Add a listener to receive events when a new Frame is available.
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
        data: unknown;
        
        /**
         * Frame type (used by event emitting).
         */
        type: string;
        
        /**
         * The current frame rate.
         */
        currentFrameRate: number;
        
        constructor(data?: unknown);
        
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
}
