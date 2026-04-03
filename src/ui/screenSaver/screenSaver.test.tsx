import { render } from '@testing-library/react';
import { ScreenSaver } from './ScreenSaver';

describe('ScreenSaver', () => {
  it('renders with visible class when shouldShow is true', () => {
    const { container } = render(<ScreenSaver shouldShow={true} />);
    expect(container.querySelector('.screen-saver')).toHaveClass('visible');
  });

  it('does not have visible class when shouldShow is false', () => {
    const { container } = render(<ScreenSaver shouldShow={false} />);
    expect(container.querySelector('.screen-saver')).not.toHaveClass('visible');
  });

  it('renders a video element', () => {
    const { container } = render(<ScreenSaver shouldShow={false} />);
    expect(container.querySelector('video')).toBeInTheDocument();
  });
});
