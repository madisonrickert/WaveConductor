import { computeStats } from './particleStats';
import { createParticle, ParticleSystem } from './particleSystem';

function makeSystemForStats(particles: ReturnType<typeof createParticle>[]) {
  const canvas = { width: 100, height: 100 } as HTMLCanvasElement;
  return {
    particles,
    canvas,
  } as unknown as ParticleSystem;
}

describe('computeStats', () => {
  it('computes correct averageX, averageY for uniform particles', () => {
    const p1 = createParticle(0, 0);
    p1.x = 10; p1.y = 20;
    const p2 = createParticle(0, 0);
    p2.x = 30; p2.y = 40;
    const stats = computeStats(makeSystemForStats([p1, p2]));
    expect(stats.averageX).toBeCloseTo(20);
    expect(stats.averageY).toBeCloseTo(30);
  });

  it('computes zero average velocity when all particles are stationary', () => {
    const p1 = createParticle(10, 10);
    const p2 = createParticle(20, 20);
    const stats = computeStats(makeSystemForStats([p1, p2]));
    expect(stats.averageVel).toBe(0);
  });

  it('computes nonzero velocity when particles have velocity', () => {
    const p1 = createParticle(0, 0);
    p1.dx = 3; p1.dy = 4; // speed = 5
    const stats = computeStats(makeSystemForStats([p1]));
    expect(stats.averageVel).toBeCloseTo(5);
  });

  it('computes variance for known particle positions', () => {
    const p1 = createParticle(0, 0);
    p1.x = 0; p1.y = 0;
    const p2 = createParticle(0, 0);
    p2.x = 10; p2.y = 0;
    const stats = computeStats(makeSystemForStats([p1, p2]));
    // averageX = 5, varianceX2 = ((0-5)^2 + (10-5)^2) / 2 = 25
    expect(stats.varianceLength).toBeGreaterThan(0);
  });

  it('flatRatio is 1 when varianceY is 0', () => {
    const p1 = createParticle(0, 0);
    p1.x = 0; p1.y = 5;
    const p2 = createParticle(0, 0);
    p2.x = 10; p2.y = 5;
    const stats = computeStats(makeSystemForStats([p1, p2]));
    // varianceY = 0 (both at y=5, average is 5)
    expect(stats.flatRatio).toBe(1);
  });
});
