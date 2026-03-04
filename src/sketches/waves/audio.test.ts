import { getDarkness } from './audio';

describe('getDarkness', () => {
  it('returns 0 at frame = 0', () => {
    expect(getDarkness(0)).toBe(0);
  });

  it('returns 1 at frame = 500 (end of ramp up)', () => {
    // frame 500: 500 % 1000 = 500, not < 500, so second branch
    // map(500 % 500 = 0, 0, 500, 1, 0) = 1
    expect(getDarkness(500)).toBe(1);
  });

  it('returns 0 at frame = 999', () => {
    // frame 999: 999 % 1000 = 999, >= 500, second branch
    // map(999 % 500 = 499, 0, 500, 1, 0) ≈ 0.002
    expect(getDarkness(999)).toBeCloseTo(0, 1);
  });

  it('returns 0.5 at frame = 250 (midpoint of ramp up)', () => {
    // map(250, 0, 500, 0, 1) = 0.5
    expect(getDarkness(250)).toBeCloseTo(0.5);
  });

  it('is periodic with period 1000', () => {
    expect(getDarkness(123)).toBeCloseTo(getDarkness(1123));
    expect(getDarkness(750)).toBeCloseTo(getDarkness(1750));
  });
});
