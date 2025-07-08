declare module 'leapjs' {
    /**
     * The InteractionBox class represents a box-shaped region completely within
     * the field of view of the Leap Motion controller.
     * 
     * Based on lib/interaction_box.js
     *
     * The interaction box is an axis-aligned rectangular prism and provides
     * normalized coordinates for hands, fingers, and tools within this box.
     * The InteractionBox class can make it easier to map positions in the
     * Leap Motion coordinate system to 2D or 3D coordinate systems used
     * for application drawing.
     *
     * The InteractionBox region is defined by a center and dimensions along the x, y, and z axes.
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
        
        constructor(data?: unknown);
        
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
}
