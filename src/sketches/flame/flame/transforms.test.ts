import * as THREE from 'three';
import { AFFINES, VARIATIONS, createInterpolatedVariation, createRouterVariation } from './transforms';

describe('AFFINES', () => {
  describe('Negate', () => {
    it('negates all components', () => {
      const p = new THREE.Vector3(1, 2, 3);
      AFFINES.Negate(p);
      expect(p.x).toBe(-1);
      expect(p.y).toBe(-2);
      expect(p.z).toBe(-3);
    });
  });

  describe('Swap', () => {
    it('swaps and averages components', () => {
      const p = new THREE.Vector3(5, 0, 0);
      AFFINES.Swap(p);
      expect(p.x).toBeCloseTo(0);     // (0 + 0) / 2.5
      expect(p.y).toBeCloseTo(2);     // (5 + 0) / 2.5
      expect(p.z).toBeCloseTo(2);     // (5 + 0) / 2.5
    });
  });

  describe('Up1', () => {
    it('increments z by 1', () => {
      const p = new THREE.Vector3(1, 2, 3);
      AFFINES.Up1(p);
      expect(p.x).toBe(1);
      expect(p.y).toBe(2);
      expect(p.z).toBe(4);
    });
  });

  describe('TowardsOriginNegativeBias', () => {
    it('transforms a known point correctly', () => {
      const p = new THREE.Vector3(1, 1, 1);
      AFFINES.TowardsOriginNegativeBias(p);
      expect(p.x).toBeCloseTo(0.25);     // (1 - 1) / 2 + 0.25
      expect(p.y).toBeCloseTo(0);         // (1 - 1) / 2
      expect(p.z).toBeCloseTo(0.5);       // 1 / 2
    });
  });
});

describe('VARIATIONS', () => {
  describe('Linear', () => {
    it('leaves the point unchanged', () => {
      const p = new THREE.Vector3(3, 4, 5);
      VARIATIONS.Linear(p);
      expect(p.x).toBe(3);
      expect(p.y).toBe(4);
      expect(p.z).toBe(5);
    });
  });

  describe('Sin', () => {
    it('applies sin to each component', () => {
      const p = new THREE.Vector3(Math.PI / 2, 0, Math.PI);
      VARIATIONS.Sin(p);
      expect(p.x).toBeCloseTo(1);
      expect(p.y).toBeCloseTo(0);
      expect(p.z).toBeCloseTo(0);
    });
  });

  describe('Spherical', () => {
    it('inverts by length squared', () => {
      const p = new THREE.Vector3(2, 0, 0);
      VARIATIONS.Spherical(p);
      expect(p.x).toBeCloseTo(0.5);     // 2 * (1/4)
      expect(p.y).toBeCloseTo(0);
      expect(p.z).toBeCloseTo(0);
    });

    it('handles zero vector without error', () => {
      const p = new THREE.Vector3(0, 0, 0);
      VARIATIONS.Spherical(p);
      expect(p.x).toBe(0);
      expect(p.y).toBe(0);
      expect(p.z).toBe(0);
    });
  });

  describe('Normalize', () => {
    it('normalizes to unit length', () => {
      const p = new THREE.Vector3(3, 4, 0);
      VARIATIONS.Normalize(p);
      expect(p.length()).toBeCloseTo(1);
      expect(p.x).toBeCloseTo(0.6);
      expect(p.y).toBeCloseTo(0.8);
    });
  });

  describe('Shrink', () => {
    it('shrinks based on exp(-lengthSq)', () => {
      const p = new THREE.Vector3(1, 0, 0);
      VARIATIONS.Shrink(p);
      // length = 1, lengthSq = 1, exp(-1) ≈ 0.3679
      expect(p.length()).toBeCloseTo(Math.exp(-1));
    });
  });
});

describe('createInterpolatedVariation', () => {
  it('applies only variationA when interpolation = 0', () => {
    const variation = createInterpolatedVariation(
      VARIATIONS.Sin,
      VARIATIONS.Linear,
      () => 0,
    );
    const p = new THREE.Vector3(Math.PI / 2, 0, 0);
    variation(p);
    expect(p.x).toBeCloseTo(1);
    expect(p.y).toBeCloseTo(0);
  });

  it('applies only variationB when interpolation = 1', () => {
    const variation = createInterpolatedVariation(
      VARIATIONS.Sin,
      VARIATIONS.Linear,
      () => 1,
    );
    const p = new THREE.Vector3(Math.PI / 2, 0, 0);
    variation(p);
    expect(p.x).toBeCloseTo(Math.PI / 2);
  });
});

describe('createRouterVariation', () => {
  it('calls vA when router returns true', () => {
    const vA = vi.fn();
    const vB = vi.fn();
    const variation = createRouterVariation(vA, vB, () => true);
    const p = new THREE.Vector3(1, 2, 3);
    variation(p);
    expect(vA).toHaveBeenCalledWith(p);
    expect(vB).not.toHaveBeenCalled();
  });

  it('calls vB when router returns false', () => {
    const vA = vi.fn();
    const vB = vi.fn();
    const variation = createRouterVariation(vA, vB, () => false);
    const p = new THREE.Vector3(1, 2, 3);
    variation(p);
    expect(vB).toHaveBeenCalledWith(p);
    expect(vA).not.toHaveBeenCalled();
  });
});
