import { Controller } from "leapjs";
import { map } from "@/math";
import { LeapConnectionStatus } from "@/leap/leapStatus";

/**
 * Wires Leap controller connection events to a status callback.
 * Returns a cleanup function that removes all listeners.
 *
 * Uses deviceEvent messages from the server (via leapjs's streamingStarted/
 * streamingStopped and deviceAttached/deviceRemoved events) to track device
 * and streaming state.
 */
export function wireLeapConnectionEvents(
    controller: Controller,
    getCallback: () => ((status: LeapConnectionStatus) => void) | undefined,
    getProtocolVersionCallback?: () => ((version: number | null) => void) | undefined,
) {
    let deviceAttached = false;
    let streaming = false;
    let frameTimeout: ReturnType<typeof setTimeout> | null = null;

    const FRAME_TIMEOUT_MS = 500;

    function clearFrameTimeout() {
        if (frameTimeout !== null) {
            clearTimeout(frameTimeout);
            frameTimeout = null;
        }
    }

    function setStreamingState(value: boolean) {
        streaming = value;
        if (value) {
            deviceAttached = true;
            getCallback()?.("streaming");
        } else {
            getCallback()?.(deviceAttached ? "device-connected" : "connected");
        }
    }

    const onConnect = () => getCallback()?.("connected");

    const onDisconnect = () => {
        clearFrameTimeout();
        deviceAttached = false;
        streaming = false;
        getCallback()?.("disconnected");
        getProtocolVersionCallback?.()?.call(null, null);
    };

    const onReady = () => {
        const version = controller.connection.protocol?.version ?? null;
        getProtocolVersionCallback?.()?.call(null, version);
    };

    const onDeviceAttached = () => {
        deviceAttached = true;
        if (!streaming) {
            getCallback()?.("device-connected");
        }
    };

    const onDeviceRemoved = () => {
        clearFrameTimeout();
        deviceAttached = false;
        streaming = false;
        getCallback()?.("connected");
    };

    const onStreamingStarted = () => {
        if (!streaming) setStreamingState(true);
    };

    const onStreamingStopped = () => {
        clearFrameTimeout();
        if (streaming) setStreamingState(false);
    };

    // Detect streaming from actual device frames, not the 'frame' event
    // which re-emits the last valid frame on every animation tick via
    // loopWhileDisconnected. Only 'deviceFrame' fires for fresh data
    // from the server.
    const onDeviceFrame = () => {
        if (!streaming) setStreamingState(true);
        clearFrameTimeout();
        frameTimeout = setTimeout(() => {
            if (streaming) setStreamingState(false);
        }, FRAME_TIMEOUT_MS);
    };

    controller
        .on('connect', onConnect)
        .on('disconnect', onDisconnect)
        .on('ready', onReady)
        .on('deviceAttached', onDeviceAttached)
        .on('deviceRemoved', onDeviceRemoved)
        .on('streamingStarted', onStreamingStarted)
        .on('streamingStopped', onStreamingStopped)
        .on('deviceFrame', onDeviceFrame);

    return () => {
        clearFrameTimeout();
        controller
            .removeListener('connect', onConnect)
            .removeListener('disconnect', onDisconnect)
            .removeListener('ready', onReady)
            .removeListener('deviceAttached', onDeviceAttached)
            .removeListener('deviceRemoved', onDeviceRemoved)
            .removeListener('streamingStarted', onStreamingStarted)
            .removeListener('streamingStopped', onStreamingStopped)
            .removeListener('deviceFrame', onDeviceFrame);
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
