import { detuned, semitone } from './tuning';

describe('detuned', () => {
  it('returns same frequency when centsOffset is 0', () => {
    expect(detuned(440, 0)).toBe(440);
  });

  it('doubles frequency at +1200 cents (one octave up)', () => {
    expect(detuned(440, 1200)).toBeCloseTo(880);
  });

  it('halves frequency at -1200 cents (one octave down)', () => {
    expect(detuned(440, -1200)).toBeCloseTo(220);
  });

  it('returns correct value for +100 cents (one semitone up)', () => {
    expect(detuned(440, 100)).toBeCloseTo(466.16, 1);
  });
});

describe('semitone', () => {
  it('returns same frequency when semitoneOffset is 0', () => {
    expect(semitone(440, 0)).toBe(440);
  });

  it('transposes up 12 semitones equals doubling', () => {
    expect(semitone(440, 12)).toBeCloseTo(880);
  });

  it('is equivalent to detuned with cents = semitones * 100', () => {
    expect(semitone(440, 7)).toBeCloseTo(detuned(440, 700));
  });
});
