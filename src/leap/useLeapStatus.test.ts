import { renderHook, act } from '@testing-library/react';
import { useLeapStatus } from './useLeapStatus';

describe('useLeapStatus', () => {
    afterEach(() => {
        delete (window as Record<string, unknown>).electronAPI;
    });

    it('returns default statuses when electronAPI is not available', () => {
        const { result } = renderHook(() => useLeapStatus());
        expect(result.current.processStatus).toBe('not-started');
        expect(result.current.connectionStatus).toBe('disconnected');
    });

    it('queries initial process status from electronAPI', async () => {
        window.electronAPI = {
            getLeapProcessStatus: vi.fn().mockResolvedValue('running'),
            onLeapProcessStatus: vi.fn().mockReturnValue(vi.fn()),
            startLeapProcess: vi.fn(),
            stopLeapProcess: vi.fn(),
        };

        const { result } = renderHook(() => useLeapStatus());

        // Wait for the async getLeapProcessStatus to resolve
        await act(async () => {});

        expect(window.electronAPI!.getLeapProcessStatus).toHaveBeenCalled();
        expect(result.current.processStatus).toBe('running');
    });

    it('subscribes to process status updates', async () => {
        let statusCallback: ((status: string) => void) | null = null;
        const cleanup = vi.fn();

        window.electronAPI = {
            getLeapProcessStatus: vi.fn().mockResolvedValue('running'),
            onLeapProcessStatus: vi.fn((cb: (status: string) => void) => {
                statusCallback = cb;
                return cleanup;
            }),
            startLeapProcess: vi.fn(),
            stopLeapProcess: vi.fn(),
        };

        const { result, unmount } = renderHook(() => useLeapStatus());
        await act(async () => {});

        expect(statusCallback).not.toBeNull();

        // Simulate a status update from main process
        act(() => {
            statusCallback!('exited');
        });
        expect(result.current.processStatus).toBe('exited');

        // Cleanup on unmount
        unmount();
        expect(cleanup).toHaveBeenCalled();
    });

    it('setConnectionStatus updates connection status', () => {
        const { result } = renderHook(() => useLeapStatus());

        act(() => {
            result.current.setConnectionStatus('streaming');
        });
        expect(result.current.connectionStatus).toBe('streaming');
    });

    it('derives process status as "external" when connected without electronAPI', () => {
        const { result } = renderHook(() => useLeapStatus());

        act(() => {
            result.current.setConnectionStatus('connected');
        });
        expect(result.current.processStatus).toBe('external');
    });

    it('derives process status as "not-started" when disconnected without electronAPI', () => {
        const { result } = renderHook(() => useLeapStatus());
        expect(result.current.processStatus).toBe('not-started');
    });

    it('startProcess calls electronAPI and updates status', async () => {
        window.electronAPI = {
            getLeapProcessStatus: vi.fn().mockResolvedValue('exited'),
            onLeapProcessStatus: vi.fn().mockReturnValue(vi.fn()),
            startLeapProcess: vi.fn().mockResolvedValue('running'),
            stopLeapProcess: vi.fn(),
        };

        const { result } = renderHook(() => useLeapStatus());
        await act(async () => {});

        await act(async () => {
            await result.current.startProcess();
        });

        expect(window.electronAPI!.startLeapProcess).toHaveBeenCalled();
        expect(result.current.processStatus).toBe('running');
    });

    it('stopProcess calls electronAPI and updates status', async () => {
        window.electronAPI = {
            getLeapProcessStatus: vi.fn().mockResolvedValue('running'),
            onLeapProcessStatus: vi.fn().mockReturnValue(vi.fn()),
            startLeapProcess: vi.fn(),
            stopLeapProcess: vi.fn().mockResolvedValue('exited'),
        };

        const { result } = renderHook(() => useLeapStatus());
        await act(async () => {});

        await act(async () => {
            await result.current.stopProcess();
        });

        expect(window.electronAPI!.stopLeapProcess).toHaveBeenCalled();
        expect(result.current.processStatus).toBe('exited');
    });

    it('startProcess is a no-op without electronAPI', async () => {
        const { result } = renderHook(() => useLeapStatus());

        // Should not throw
        await act(async () => {
            await result.current.startProcess();
        });
        expect(result.current.processStatus).toBe('not-started');
    });
});
