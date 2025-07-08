declare module 'leapjs' {
    /**
     * Finger class extending Pointable.
     * Based on lib/finger.js
     */
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
        
        constructor(data?: unknown);
        
        /**
         * A string containing a brief, human readable description of the Finger object.
         */
        toString(): string;
        
        /**
         * An invalid Finger object.
         */
        static Invalid: Finger;
    }
}
