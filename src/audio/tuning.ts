/** Detune a frequency by a given number of cents. */
export function detuned(freq: number, centsOffset: number) {
    return freq * Math.pow(2, centsOffset / 1200);
}

/** Transpose a frequency by a given number of semitones. */
export function semitone(freq: number, semitoneOffset: number) {
    return detuned(freq, semitoneOffset * 100);
}
