import * as THREE from 'three';
import { VelocityTrackerVisitor, LengthVarianceTrackerVisitor, BoxCountVisitor } from './updateVisitor';
import type { SuperPoint } from './superPoint';

function makeSuperPoint(x: number, y: number, z: number, lastX = 0, lastY = 0, lastZ = 0): SuperPoint {
  return {
    point: new THREE.Vector3(x, y, z),
    lastPoint: new THREE.Vector3(lastX, lastY, lastZ),
  } as SuperPoint;
}

describe('VelocityTrackerVisitor', () => {
  it('returns 0 when no points visited', () => {
    const visitor = new VelocityTrackerVisitor();
    expect(visitor.computeVelocity()).toBe(0);
  });

  it('computes average velocity over multiple points', () => {
    const visitor = new VelocityTrackerVisitor();
    // point at (1,0,0), lastPoint at (0,0,0) => distance = 1
    visitor.visit(makeSuperPoint(1, 0, 0, 0, 0, 0));
    // point at (0,3,4), lastPoint at (0,0,0) => distance = 5
    visitor.visit(makeSuperPoint(0, 3, 4, 0, 0, 0));
    expect(visitor.computeVelocity()).toBeCloseTo(3); // (1 + 5) / 2
  });
});

describe('LengthVarianceTrackerVisitor', () => {
  it('returns 0 when no points visited', () => {
    const visitor = new LengthVarianceTrackerVisitor();
    expect(visitor.computeVariance()).toBe(0);
  });

  it('returns 0 when all points are at the same distance from origin', () => {
    const visitor = new LengthVarianceTrackerVisitor();
    // All at distance 1 from origin
    visitor.visit(makeSuperPoint(1, 0, 0));
    visitor.visit(makeSuperPoint(0, 1, 0));
    visitor.visit(makeSuperPoint(0, 0, 1));
    expect(visitor.computeVariance()).toBeCloseTo(0);
  });

  it('computes nonzero variance for points at different distances', () => {
    const visitor = new LengthVarianceTrackerVisitor();
    visitor.visit(makeSuperPoint(1, 0, 0));  // length = 1
    visitor.visit(makeSuperPoint(3, 0, 0));  // length = 3
    expect(visitor.computeVariance()).toBeGreaterThan(0);
  });
});

describe('BoxCountVisitor', () => {
  it('counts distinct boxes for points in different boxes', () => {
    const visitor = new BoxCountVisitor([1]);
    visitor.visit(makeSuperPoint(0.5, 0.5, 0.5));
    visitor.visit(makeSuperPoint(1.5, 0.5, 0.5));
    expect(visitor.counts[0]).toBe(2);
  });

  it('counts single box when all points are in the same box', () => {
    const visitor = new BoxCountVisitor([2]);
    visitor.visit(makeSuperPoint(0.5, 0.5, 0.5));
    visitor.visit(makeSuperPoint(0.7, 0.3, 0.1));
    expect(visitor.counts[0]).toBe(1);
  });

  it('handles multiple side lengths simultaneously', () => {
    const visitor = new BoxCountVisitor([1, 10]);
    visitor.visit(makeSuperPoint(0.5, 0.5, 0.5));
    visitor.visit(makeSuperPoint(1.5, 0.5, 0.5));
    // side=1: two different boxes; side=10: same box
    expect(visitor.counts[0]).toBe(2);
    expect(visitor.counts[1]).toBe(1);
  });

  it('density increases with repeated visits to same box', () => {
    const visitor = new BoxCountVisitor([10]);
    visitor.visit(makeSuperPoint(0.5, 0.5, 0.5));
    const densityAfterOne = visitor.densities[0];
    visitor.visit(makeSuperPoint(0.7, 0.3, 0.1));
    expect(visitor.densities[0]).toBeGreaterThan(densityAfterOne);
  });
});
