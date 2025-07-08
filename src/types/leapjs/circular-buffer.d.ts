declare module 'leapjs' {
    /**
     * Circular buffer implementation for frame history.
     * Based on lib/circular_buffer.js
     */
    export class CircularBuffer<T> {
        constructor(size: number);
        get(index: number): T;
        push(item: T): void;
    }
}
