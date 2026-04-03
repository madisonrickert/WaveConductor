export type LeapProcessStatus = "not-started" | "running" | "errored" | "exited" | "external";

/**
 * Connection status as seen from the WebSocket client.
 *
 * The UltraleapTrackingWebSocket server sends deviceEvent messages that let us
 * distinguish device attachment and streaming states.
 *
 * Server scenario                               | WS Connected | deviceEvent      | State
 * ----------------------------------------------|--------------|------------------|------------------
 * WS server not running                         | no           | —                | disconnected
 * WS server up, no device attached              | yes          | —                | connected
 * WS server up, device attached, not streaming  | yes          | attached=true    | device-connected
 * Device attached and streaming                  | yes          | streaming=true   | streaming
 */
export type LeapConnectionStatus = "disconnected" | "connected" | "device-connected" | "streaming";
