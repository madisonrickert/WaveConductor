declare module 'leapjs' {
    /**
     * Hand class representing a detected hand.
     * Based on lib/hand.js
     */
    export class Hand {
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
        constructor(data?: unknown);

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
    }
}
