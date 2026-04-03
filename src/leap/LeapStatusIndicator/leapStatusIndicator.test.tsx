import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { LeapStatusIndicator } from './LeapStatusIndicator';
import type { LeapProcessStatus, LeapConnectionStatus } from '@/leap/leapStatus';

interface IndicatorOverrides {
    processStatus?: LeapProcessStatus;
    connectionStatus?: LeapConnectionStatus;
    protocolVersion?: number | null;
    onStart?: () => void;
    onStop?: () => void;
}

function renderIndicator(overrides: IndicatorOverrides = {}) {
    const props = {
        processStatus: "running" as LeapProcessStatus,
        connectionStatus: "disconnected" as LeapConnectionStatus,
        protocolVersion: null,
        onStart: vi.fn(),
        onStop: vi.fn(),
        ...overrides,
    };
    return render(<LeapStatusIndicator {...props} />);
}

describe('LeapStatusIndicator', () => {
    describe('dot appearance', () => {
        it('shows disconnected dot when connection is disconnected', () => {
            renderIndicator({ connectionStatus: "disconnected" });
            const dot = screen.getByRole('button', { name: /ultraleap/i });
            expect(dot).toHaveClass('disconnected');
        });

        it('shows disconnected dot when connection is connected but not streaming', () => {
            renderIndicator({ connectionStatus: "connected" });
            const dot = screen.getByRole('button', { name: /ultraleap/i });
            expect(dot).toHaveClass('disconnected');
        });

        it('shows connected dot when connection is streaming', () => {
            renderIndicator({ connectionStatus: "streaming" });
            const dot = screen.getByRole('button', { name: /ultraleap/i });
            expect(dot).toHaveClass('connected');
        });
    });

    describe('tooltip', () => {
        it('shows "Ultraleap: Disconnected" in title when disconnected', () => {
            renderIndicator({ connectionStatus: "disconnected" });
            expect(screen.getByTitle('Ultraleap: Disconnected')).toBeInTheDocument();
        });

        it('shows "Ultraleap: Server Only" in title when connected but not streaming', () => {
            renderIndicator({ connectionStatus: "connected" });
            expect(screen.getByTitle('Ultraleap: Server Only')).toBeInTheDocument();
        });

        it('shows "Ultraleap: Streaming" in title when streaming', () => {
            renderIndicator({ connectionStatus: "streaming" });
            expect(screen.getByTitle('Ultraleap: Streaming')).toBeInTheDocument();
        });
    });

    describe('expanded panel', () => {
        it('does not show panel by default', () => {
            renderIndicator();
            expect(screen.queryByText('Ultraleap Status')).not.toBeInTheDocument();
        });

        it('shows panel when dot is clicked', async () => {
            renderIndicator();
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Ultraleap Status')).toBeInTheDocument();
        });

        it('hides panel when dot is clicked again', async () => {
            renderIndicator();
            const dot = screen.getByRole('button', { name: /ultraleap/i });
            await userEvent.click(dot);
            expect(screen.getByText('Ultraleap Status')).toBeInTheDocument();
            await userEvent.click(dot);
            expect(screen.queryByText('Ultraleap Status')).not.toBeInTheDocument();
        });

        it('displays process status label', async () => {
            renderIndicator({ processStatus: "running" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Running')).toBeInTheDocument();
        });

        it('displays connection status label when streaming', async () => {
            renderIndicator({ connectionStatus: "streaming" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Streaming')).toBeInTheDocument();
        });

        it('displays "Server Only" when connected without device', async () => {
            renderIndicator({ connectionStatus: "connected" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Server Only')).toBeInTheDocument();
        });

        it('displays errored process status', async () => {
            renderIndicator({ processStatus: "errored" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Errored')).toBeInTheDocument();
        });

        it('displays external process status', async () => {
            renderIndicator({ processStatus: "external" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('External')).toBeInTheDocument();
        });
    });

    describe('toggle button', () => {
        it('shows "Stop Server" when process is running', async () => {
            renderIndicator({ processStatus: "running" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Stop Server')).toBeInTheDocument();
        });

        it('shows "Start Server" when process has exited', async () => {
            renderIndicator({ processStatus: "exited" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Start Server')).toBeInTheDocument();
        });

        it('calls onStop when "Stop Server" is clicked', async () => {
            const onStop = vi.fn();
            renderIndicator({ processStatus: "running", onStop });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            await userEvent.click(screen.getByText('Stop Server'));
            expect(onStop).toHaveBeenCalledTimes(1);
        });

        it('calls onStart when "Start Server" is clicked for exited process', async () => {
            const onStart = vi.fn();
            renderIndicator({ processStatus: "exited", onStart });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            await userEvent.click(screen.getByText('Start Server'));
            expect(onStart).toHaveBeenCalledTimes(1);
        });

        it('does not show toggle button when process is external', async () => {
            renderIndicator({ processStatus: "external" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.queryByText('Stop Server')).not.toBeInTheDocument();
            expect(screen.queryByText('Start Server')).not.toBeInTheDocument();
        });

        it('does not show toggle button when process is not-started', async () => {
            renderIndicator({ processStatus: "not-started" });
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.queryByText('Stop Server')).not.toBeInTheDocument();
            expect(screen.queryByText('Start Server')).not.toBeInTheDocument();
        });
    });

    describe('outside click', () => {
        it('closes panel when clicking outside', async () => {
            renderIndicator();
            await userEvent.click(screen.getByRole('button', { name: /ultraleap/i }));
            expect(screen.getByText('Ultraleap Status')).toBeInTheDocument();
            // Click outside the component
            await userEvent.click(document.body);
            expect(screen.queryByText('Ultraleap Status')).not.toBeInTheDocument();
        });
    });
});
