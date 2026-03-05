import * as THREE from 'three';
import { applyBranch, Branch } from './branch';

describe('applyBranch', () => {
  const makeBranch = (color = new THREE.Color(0.1, 0.2, 0.3)): Branch => ({
    color,
    affine: (p) => p.multiplyScalar(2),
    variation: (p) => p.addScalar(1),
  });

  it('applies affine transform to the point', () => {
    const branch = makeBranch();
    const point = new THREE.Vector3(1, 2, 3);
    const color = new THREE.Color(0, 0, 0);
    applyBranch(branch, point, color);
    // affine doubles: (1,2,3) -> (2,4,6), variation adds 1: -> (3,5,7)
    expect(point.x).toBe(3);
    expect(point.y).toBe(5);
    expect(point.z).toBe(7);
  });

  it('adds branch color to the input color', () => {
    const branch = makeBranch(new THREE.Color(0.1, 0.2, 0.3));
    const point = new THREE.Vector3(0, 0, 0);
    const color = new THREE.Color(0.5, 0.5, 0.5);
    applyBranch(branch, point, color);
    expect(color.r).toBeCloseTo(0.6);
    expect(color.g).toBeCloseTo(0.7);
    expect(color.b).toBeCloseTo(0.8);
  });
});
