declare module 'leapjs' {
    /**
     * Pointable class representing fingers and tools.
     * Based on lib/pointable.js
     */
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
        
        constructor(data?: unknown);
        
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
    }
}
