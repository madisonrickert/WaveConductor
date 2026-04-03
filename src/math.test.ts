import { lerp, map, sampleArray, triangleWaveApprox, mod, mirroredRepeat, logistic } from './math';

describe('lerp', () => {
  it('returns a when x = 0', () => {
    expect(lerp(3, 7, 0)).toBe(3);
  });

  it('returns b when x = 1', () => {
    expect(lerp(3, 7, 1)).toBe(7);
  });

  it('returns midpoint when x = 0.5', () => {
    expect(lerp(0, 10, 0.5)).toBe(5);
  });

  it('extrapolates beyond [0, 1]', () => {
    expect(lerp(0, 10, 2)).toBe(20);
  });
});

describe('map', () => {
  it('maps xStart to yStart', () => {
    expect(map(0, 0, 10, 100, 200)).toBe(100);
  });

  it('maps xStop to yStop', () => {
    expect(map(10, 0, 10, 100, 200)).toBe(200);
  });

  it('maps midpoint to midpoint', () => {
    expect(map(5, 0, 10, 100, 200)).toBe(150);
  });

  it('handles inverted ranges', () => {
    expect(map(5, 0, 10, 200, 100)).toBe(150);
  });
});

describe('sampleArray', () => {
  it('returns an element from the array', () => {
    const arr = [10, 20, 30];
    expect(arr).toContain(sampleArray(arr));
  });

  it('returns the only element for single-element array', () => {
    expect(sampleArray([42])).toBe(42);
  });
});

describe('triangleWaveApprox', () => {
  it('returns approximately 1 at t = PI/2', () => {
    // Only 3-term Fourier approximation, so accuracy is ~0.93
    expect(triangleWaveApprox(Math.PI / 2)).toBeCloseTo(0.933, 2);
  });

  it('returns approximately -1 at t = 3*PI/2', () => {
    expect(triangleWaveApprox(3 * Math.PI / 2)).toBeCloseTo(-0.933, 2);
  });

  it('returns approximately 0 at t = 0', () => {
    expect(triangleWaveApprox(0)).toBeCloseTo(0, 5);
  });

  it('returns approximately 0 at t = PI', () => {
    expect(triangleWaveApprox(Math.PI)).toBeCloseTo(0, 1);
  });
});

describe('mod', () => {
  it('handles positive values', () => {
    expect(mod(5, 3)).toBe(2);
  });

  it('handles negative values', () => {
    expect(mod(-1, 3)).toBe(2);
  });

  it('handles zero', () => {
    expect(mod(0, 5)).toBe(0);
  });
});

describe('mirroredRepeat', () => {
  it('returns 0 at x = 0', () => {
    expect(mirroredRepeat(0)).toBeCloseTo(0);
  });

  it('returns 1 at x = 1', () => {
    expect(mirroredRepeat(1)).toBeCloseTo(1);
  });

  it('returns 0 at x = 2', () => {
    expect(mirroredRepeat(2)).toBeCloseTo(0);
  });

  it('is periodic: value at x equals value at x + 2', () => {
    expect(mirroredRepeat(0.7)).toBeCloseTo(mirroredRepeat(2.7));
  });
});

describe('logistic', () => {
  it('returns 0.5 at x = 0', () => {
    expect(logistic(0)).toBe(0.5);
  });

  it('returns 0 for x < -6', () => {
    expect(logistic(-7)).toBe(0);
  });

  it('returns 1 for x > 6', () => {
    expect(logistic(7)).toBe(1);
  });

  it('is monotonically increasing', () => {
    expect(logistic(1)).toBeGreaterThan(logistic(0));
    expect(logistic(2)).toBeGreaterThan(logistic(1));
  });

  it('is symmetric around 0', () => {
    expect(logistic(2) + logistic(-2)).toBeCloseTo(1);
  });
});
