import { render, screen } from '@testing-library/react';
import { HandOverlay, HandData } from './index';

describe('HandOverlay', () => {
  it('renders nothing when hands array is empty', () => {
    const { container } = render(<HandOverlay hands={[]} />);
    expect(container.querySelectorAll('.hand-cursor')).toHaveLength(0);
  });

  it('renders a hand cursor for each hand', () => {
    const hands: HandData[] = [
      { index: 0, position: { x: 100, y: 200 }, pinched: false },
      { index: 1, position: { x: 300, y: 400 }, pinched: true },
    ];
    const { container } = render(<HandOverlay hands={hands} />);
    expect(container.querySelectorAll('.hand-cursor')).toHaveLength(2);
  });

  it('positions hand cursor at correct coordinates', () => {
    const hands: HandData[] = [
      { index: 0, position: { x: 100, y: 200 }, pinched: false },
    ];
    const { container } = render(<HandOverlay hands={hands} />);
    const cursor = container.querySelector('.hand-cursor') as HTMLElement;
    expect(cursor.style.left).toBe('100px');
    expect(cursor.style.top).toBe('200px');
  });
});
