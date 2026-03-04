import { Controller } from "leapjs";
import { map } from "@/common/math";
import { LeapConnectionStatus } from "@/common/leapStatus";

/**
 * Wires Leap controller connection events to a status callback.
 * Returns a cleanup function that removes all listeners.
 *
 * Streaming detection: Our UltraleapTrackingWebSocket server reports protocol
 * v6 but doesn't send deviceEvent messages, so leapjs's built-in
 * streamingStarted/streamingStopped events never fire. Instead, we detect
 * streaming by monitoring actual frames on the connection (not controller —
 * connection frames are server-only, not synthetic animation frames).
 */
export function wireLeapConnectionEvents(
    controller: Controller,
    getCallback: () => ((status: LeapConnectionStatus) => void) | undefined,
    getProtocolVersionCallback?: () => ((version: number | null) => void) | undefined,
) {
    const STREAMING_TIMEOUT_MS = 3000;
    let streamingTimeout: ReturnType<typeof setTimeout> | null = null;
    let isStreaming = false;

    const onConnect = () => getCallback()?.("connected");

    const onDisconnect = () => {
        isStreaming = false;
        if (streamingTimeout) { clearTimeout(streamingTimeout); streamingTimeout = null; }
        getCallback()?.("disconnected");
        getProtocolVersionCallback?.()?.call(null, null);
    };

    const onReady = () => {
        const version = controller.connection.protocol?.version ?? null;
        getProtocolVersionCallback?.()?.call(null, version);
    };

    const onConnectionFrame = () => {
        if (!isStreaming) {
            isStreaming = true;
            getCallback()?.("streaming");
        }
        if (streamingTimeout) clearTimeout(streamingTimeout);
        streamingTimeout = setTimeout(() => {
            if (isStreaming) {
                isStreaming = false;
                getCallback()?.("connected");
            }
        }, STREAMING_TIMEOUT_MS);
    };

    controller
        .on('connect', onConnect)
        .on('disconnect', onDisconnect)
        .on('ready', onReady);
    controller.connection.on('frame', onConnectionFrame);

    return () => {
        if (streamingTimeout) { clearTimeout(streamingTimeout); streamingTimeout = null; }
        controller
            .removeListener('connect', onConnect)
            .removeListener('disconnect', onDisconnect)
            .removeListener('ready', onReady);
        controller.connection.removeListener('frame', onConnectionFrame);
    };
}

const LEAP_RANGE_MIN = 0.2;
const LEAP_RANGE_MAX = 0.8;

export function mapLeapToThreePosition(canvas: HTMLCanvasElement, position: number[]) {
    // position[0] is left/right; left is negative, right is positive. each unit is one millimeter
    const x = map(position[0], -200, 200, canvas.width * LEAP_RANGE_MIN,  canvas.width * LEAP_RANGE_MAX);
    // 40 is about 4cm, 1 inch, to 35cm = 13 inches above
    const y = map(position[1], 350, 40,   canvas.height * LEAP_RANGE_MIN, canvas.height * LEAP_RANGE_MAX);
    return { x, y };
}
