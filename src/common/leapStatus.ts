export type LeapProcessStatus = "not-started" | "running" | "errored" | "exited" | "external";

/**
 * Connection status as seen from the WebSocket client.
 *
 * The UltraleapTrackingWebSocket server always starts regardless of Ultraleap
 * software availability. It always sends {"version":6} on connect. The only
 * difference is whether tracking frame data flows.
 *
 * Server scenario                               | WS Connected | Frames | State
 * ----------------------------------------------|--------------|--------|-------------
 * WS server not running                         | no           | —      | disconnected
 * WS server up, Ultraleap software not running  | yes          | no     | connected
 * WS server up, software running, no device     | yes          | no     | connected
 * Software + device, no hands in view           | yes          | yes    | streaming
 * Software + device + hands in view             | yes          | yes    | streaming
 *
 * Note: "no Ultraleap software" and "no device" are indistinguishable from
 * the WS client — both appear as "connected" (server only, no frame data).
 */
export type LeapConnectionStatus = "disconnected" | "connected" | "streaming";
