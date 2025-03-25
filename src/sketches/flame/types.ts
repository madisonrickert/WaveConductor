export interface Chord {
    root: OscillatorNode;
    third: OscillatorNode;
    fifth: OscillatorNode;
    gain: GainNode;
    setIsMajor: (major: boolean) => void;
    setScaleDegree: (scaleDegree: number) => void;
    setMinorBias: (minorBias: number) => void;
    setFifthBias: (fifthBias: number) => void;
}