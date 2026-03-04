import * as THREE from 'three';
import { createParticle, ParticleSystem, IParticle, ParticleSystemParameters } from './particleSystem';

describe('createParticle', () => {
  it('creates particle at original position with zero velocity', () => {
    const p = createParticle(10, 20);
    expect(p.x).toBe(10);
    expect(p.y).toBe(20);
    expect(p.originalX).toBe(10);
    expect(p.originalY).toBe(20);
    expect(p.dx).toBe(0);
    expect(p.dy).toBe(0);
  });

  it('initializes color as white with alpha 0', () => {
    const p = createParticle(0, 0);
    expect(p.color.x).toBe(1);
    expect(p.color.y).toBe(1);
    expect(p.color.z).toBe(1);
    expect(p.color.w).toBe(0);
  });
});

function makeSystem(particles: IParticle[], paramsOverrides: Partial<ParticleSystemParameters> = {}) {
  const canvas = { width: 100, height: 100 } as HTMLCanvasElement;
  const params: ParticleSystemParameters = {
    GRAVITY_CONSTANT: 280,
    timeStep: 1 / 60,
    PULLING_DRAG_CONSTANT: 0.98,
    INERTIAL_DRAG_CONSTANT: 0.9,
    STATIONARY_CONSTANT: 0.01,
    FADE_DURATION: 1,
    constrainToBox: false,
    ...paramsOverrides,
  };
  return new ParticleSystem(canvas, particles, params);
}

function makePointCloud(numParticles: number) {
  const positions = new Float32Array(numParticles * 3);
  const colors = new Float32Array(numParticles * 4);
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geometry.setAttribute('color', new THREE.BufferAttribute(colors, 4));
  return new THREE.Points(geometry);
}

describe('ParticleSystem', () => {
  describe('resetToOriginalPosition', () => {
    it('resets x, y to originalX, originalY and zeros velocity', () => {
      const p = createParticle(10, 20);
      p.x = 50;
      p.y = 60;
      p.dx = 5;
      p.dy = 5;
      p.color.w = 0.8;

      const system = makeSystem([p]);
      system.resetToOriginalPosition(p);

      expect(p.x).toBe(10);
      expect(p.y).toBe(20);
      expect(p.dx).toBe(0);
      expect(p.dy).toBe(0);
      expect(p.color.w).toBe(0);
    });
  });

  describe('stepParticles', () => {
    it('with no attractors, particles drift toward original position', () => {
      const p = createParticle(50, 50);
      p.x = 60;
      p.y = 50;
      const system = makeSystem([p], { STATIONARY_CONSTANT: 1 });
      const pointCloud = makePointCloud(1);

      system.stepParticles([], pointCloud);

      // Should have acquired velocity back toward original position
      expect(p.dx).toBeLessThan(0); // moving left toward originalX = 50
    });

    it('fades alpha toward 1 over time', () => {
      const p = createParticle(50, 50);
      expect(p.color.w).toBe(0);
      const system = makeSystem([p], { FADE_DURATION: 1 });
      const pointCloud = makePointCloud(1);

      system.stepParticles([], pointCloud);

      expect(p.color.w).toBeGreaterThan(0);
      expect(p.color.w).toBeLessThanOrEqual(1);
    });

    it('updates position buffer attribute', () => {
      const p = createParticle(50, 50);
      p.dx = 10;
      const system = makeSystem([p]);
      const pointCloud = makePointCloud(1);

      system.stepParticles([], pointCloud);

      const posAttr = pointCloud.geometry.getAttribute('position');
      // Position should have been updated
      expect(posAttr.getX(0)).not.toBe(0);
    });

    it('constrains particles to box when enabled', () => {
      const p = createParticle(50, 50);
      p.x = -10; // Outside canvas bounds
      const system = makeSystem([p], { constrainToBox: true, STATIONARY_CONSTANT: 0 });
      const pointCloud = makePointCloud(1);

      system.stepParticles([], pointCloud);

      // Particle should be reset to original position
      expect(p.x).toBe(50);
      expect(p.y).toBe(50);
    });

    it('does not constrain to box when disabled', () => {
      const p = createParticle(50, 50);
      p.x = -10;
      const system = makeSystem([p], { constrainToBox: false, STATIONARY_CONSTANT: 0 });
      const pointCloud = makePointCloud(1);

      system.stepParticles([], pointCloud);

      // Particle should stay out of bounds
      expect(p.x).toBeLessThan(0);
    });
  });
});
