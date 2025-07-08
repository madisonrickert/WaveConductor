declare module 'leapjs' {
    /**
     * Bone class representing finger and hand bones.
     * Based on lib/bone.js
     */
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
         * The center position of the bone
         * Returns a vec3 array.
         */
        center(): number[];
        
        /**
         * The negative of the z-basis
         */
        direction(): number[];
    }
}
