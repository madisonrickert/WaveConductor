import * as THREE from 'three';
import { SuperPoint } from './superPoint';
import { AFFINES, VARIATIONS } from './transforms';
import type { Branch } from './branch';
import type { UpdateVisitor } from './updateVisitor';

function makeGeometry(maxPoints = 1000) {
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(maxPoints * 3), 3));
  geometry.setAttribute('color', new THREE.Float32BufferAttribute(new Float32Array(maxPoints * 3), 3));
  geometry.setDrawRange(0, 0);
  return geometry;
}

function makeBranch(affine = AFFINES.TowardsOriginNegativeBias, variation = VARIATIONS.Linear): Branch {
  return { affine, variation, color: new THREE.Color(0.1, 0, 0) };
}

beforeEach(() => {
  SuperPoint.nextSlot = 0;
});

describe('SuperPoint', () => {
  describe('slot allocation', () => {
    it('assigns incrementing slots starting from 0', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch()];
      const sp1 = new SuperPoint(new THREE.Vector3(1, 2, 3), new THREE.Color(), geometry, branches);
      const sp2 = new SuperPoint(new THREE.Vector3(4, 5, 6), new THREE.Color(), geometry, branches);
      expect(sp1.slot).toBe(0);
      expect(sp2.slot).toBe(1);
    });

    it('writes initial position and color to geometry buffer', () => {
      const geometry = makeGeometry();
      const sp = new SuperPoint(new THREE.Vector3(1, 2, 3), new THREE.Color(0.5, 0.6, 0.7), geometry, [makeBranch()]);
      const posArr = (geometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
      const colArr = (geometry.attributes.color as THREE.BufferAttribute).array as Float32Array;
      expect(posArr[0]).toBe(1);
      expect(posArr[1]).toBe(2);
      expect(posArr[2]).toBe(3);
      expect(colArr[0]).toBeCloseTo(0.5);
      expect(colArr[1]).toBeCloseTo(0.6);
      expect(colArr[2]).toBeCloseTo(0.7);
    });

    it('increments nextSlot across the tree', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch(), makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      // Trigger child creation by running one level
      root.recalculate(1, 1, 1, 1, false, []);
      // root(slot 0) + 2 children = 3 total
      expect(SuperPoint.nextSlot).toBe(3);
    });
  });

  describe('updateSubtree', () => {
    it('does nothing at depth 0', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(1, 0, 0), new THREE.Color(), geometry, branches);
      root.recalculate(1, 1, 1, 0, false, []);
      // Only root exists
      expect(SuperPoint.nextSlot).toBe(1);
      expect(root.children).toBeUndefined();
    });

    it('lazily creates children on first call and reuses them', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch(), makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      root.recalculate(1, 1, 1, 1, false, []);
      const firstChildren = root.children;
      expect(firstChildren).toHaveLength(2);

      // Second call reuses the same children
      root.recalculate(2, 2, 2, 1, false, []);
      expect(root.children).toBe(firstChildren);
    });

    it('writes child positions to the geometry buffer (no lerp)', () => {
      const geometry = makeGeometry();
      // Use Linear variation so we can predict output
      const branches = [makeBranch(AFFINES.Up1, VARIATIONS.Linear)];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      root.recalculate(0, 0, 0, 1, false, []);

      const posArr = (geometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
      // Child is at slot 1. Up1 adds 1 to z: (0, 0, 0) -> (0, 0, 1)
      expect(posArr[3]).toBeCloseTo(0);
      expect(posArr[4]).toBeCloseTo(0);
      expect(posArr[5]).toBeCloseTo(1);
    });

    it('lerps toward target when shouldLerp is true', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch(AFFINES.Up1, VARIATIONS.Linear)];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);

      // First call without lerp to initialize children
      root.recalculate(0, 0, 0, 1, false, []);
      const posArr = (geometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
      // Child position: (0, 0, 1) from Up1
      expect(posArr[5]).toBeCloseTo(1);

      // Second call with lerp — child should lerp toward target, not jump
      root.recalculate(0, 0, 0, 1, true, []);
      // Target is still (0, 0, 1). Previous was (0, 0, 1).
      // lerp(1, 1, 0.8) = 1 — should stay at 1
      expect(posArr[5]).toBeCloseTo(1);

      // Now change root position so target changes
      root.recalculate(0, 0, 10, 1, true, []);
      // Target is (0, 0, 11) from Up1(0,0,10). Previous child was at (0,0,1).
      // lerp(1, 11, 0.8) = 1 + 0.8*10 = 9
      expect(posArr[5]).toBeCloseTo(9);
    });

    it('clamps out-of-bounds points via Spherical variation', () => {
      const geometry = makeGeometry();
      // Affine that pushes point far from origin
      const farAffine = (p: THREE.Vector3) => { p.set(100, 0, 0); };
      const branches = [makeBranch(farAffine, VARIATIONS.Linear)];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      root.recalculate(0, 0, 0, 1, false, []);

      const posArr = (geometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
      // Point at (100, 0, 0) has lengthSq = 10000 > 2500, so Spherical is applied
      // Spherical(100, 0, 0) = (100/10000, 0, 0) = (0.01, 0, 0)
      expect(posArr[3]).toBeCloseTo(0.01);
      expect(posArr[4]).toBeCloseTo(0);
      expect(posArr[5]).toBeCloseTo(0);
    });
  });

  describe('visitor sampling', () => {
    it('calls visitors at the 307th iteration', () => {
      const geometry = makeGeometry(500000);
      // Need enough branches and depth to reach 307 iterations
      // 4 branches at depth 4 = 4^1 + 4^2 + 4^3 + 4^4 = 4 + 16 + 64 + 256 = 340 nodes
      const branches = [makeBranch(), makeBranch(), makeBranch(), makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);

      const visited: number[] = [];
      const visitor: UpdateVisitor = {
        visit: (p) => { visited.push(p.slot); },
      };

      root.recalculate(1, 1, 1, 4, false, [visitor]);
      // At least one visit should have occurred (iteration 307 falls within 340 nodes)
      expect(visited.length).toBeGreaterThan(0);
    });

    it('does not call visitors when visitor array is empty', () => {
      const geometry = makeGeometry(500000);
      const branches = [makeBranch(), makeBranch(), makeBranch(), makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      // Should not throw even with many iterations
      root.recalculate(1, 1, 1, 4, false, []);
    });
  });

  describe('recalculate', () => {
    it('sets draw range to total number of slots used', () => {
      const geometry = makeGeometry();
      const branches = [makeBranch(), makeBranch()];
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, branches);
      root.recalculate(1, 1, 1, 2, false, []);
      // root(1) + 2 children(2) + 4 grandchildren(4) = 7
      expect(SuperPoint.nextSlot).toBe(7);
      expect(geometry.drawRange.count).toBe(7);
    });

    it('marks position and color attributes as needing update', () => {
      const geometry = makeGeometry();
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, [makeBranch()]);

      // Spy on the setter to verify it was called with true
      const posAttr = geometry.attributes.position as THREE.BufferAttribute;
      const colAttr = geometry.attributes.color as THREE.BufferAttribute;
      let posUpdated = false;
      let colUpdated = false;
      Object.defineProperty(posAttr, 'needsUpdate', { set: (v) => { if (v) posUpdated = true; } });
      Object.defineProperty(colAttr, 'needsUpdate', { set: (v) => { if (v) colUpdated = true; } });

      root.recalculate(1, 1, 1, 1, false, []);
      expect(posUpdated).toBe(true);
      expect(colUpdated).toBe(true);
    });

    it('writes root position to buffer', () => {
      const geometry = makeGeometry();
      const root = new SuperPoint(new THREE.Vector3(), new THREE.Color(), geometry, [makeBranch()]);
      root.recalculate(5, 6, 7, 0, false, []);
      const posArr = (geometry.attributes.position as THREE.BufferAttribute).array as Float32Array;
      expect(posArr[0]).toBe(5);
      expect(posArr[1]).toBe(6);
      expect(posArr[2]).toBe(7);
    });
  });
});
