/* eslint-disable @typescript-eslint/no-explicit-any */

export interface Constructor<T> {
    new(...args: any[]): T;
    displayName: string;

    diffusionWater?: number;
    diffusionSugar?: number;
    turnsToBuild?: number;
}
