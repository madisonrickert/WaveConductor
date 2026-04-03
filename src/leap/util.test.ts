import { mapLeapToThreePosition } from './util';

describe('mapLeapToThreePosition', () => {
  const canvas = { width: 1000, height: 800 } as HTMLCanvasElement;

  it('maps center of Leap range to center of canvas', () => {
    const { x, y } = mapLeapToThreePosition(canvas, [0, 195, 0]);
    // x: map(0, -200, 200, 200, 800) = 500
    expect(x).toBeCloseTo(500);
    // y: map(195, 350, 40, 160, 640) = midpoint
    expect(y).toBeCloseTo(400);
  });

  it('maps left edge of Leap range', () => {
    const { x } = mapLeapToThreePosition(canvas, [-200, 195, 0]);
    // map(-200, -200, 200, 200, 800) = 200
    expect(x).toBeCloseTo(200);
  });

  it('maps right edge of Leap range', () => {
    const { x } = mapLeapToThreePosition(canvas, [200, 195, 0]);
    // map(200, -200, 200, 200, 800) = 800
    expect(x).toBeCloseTo(800);
  });

  it('maps y range correctly (inverted)', () => {
    const { y: yHigh } = mapLeapToThreePosition(canvas, [0, 350, 0]);
    const { y: yLow } = mapLeapToThreePosition(canvas, [0, 40, 0]);
    // High Leap Y (350) maps to canvas.height * 0.2 = 160
    expect(yHigh).toBeCloseTo(160);
    // Low Leap Y (40) maps to canvas.height * 0.8 = 640
    expect(yLow).toBeCloseTo(640);
  });
});
