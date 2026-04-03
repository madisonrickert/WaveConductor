import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { VolumeButton } from './VolumeButton';

describe('VolumeButton', () => {
  it('renders a button with the user-volume class', () => {
    render(<VolumeButton volumeEnabled onClick={() => {}} />);
    expect(screen.getByRole('button')).toHaveClass('user-volume');
  });

  it('calls onClick when clicked', async () => {
    const onClick = vi.fn();
    render(<VolumeButton volumeEnabled onClick={onClick} />);
    await userEvent.click(screen.getByRole('button'));
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
