// Type definitions for EventEmitter (re-exported from Node.js events)
import EventEmitter from "events";

declare module 'leapjs' {
    /**
     * Header object passed to protocol selection
     * Based on lib/protocol.js chooseProtocol function
     */
    export interface ProtocolHeader {
        version: number;
        serviceVersion?: string;
    }

    /**
     * Event data structure for Leap Motion events
     * Based on lib/protocol.js Event constructor
     */
    export interface EventData {
        type: string;
        state: Record<string, unknown>;
    }

    /**
     * Frame data structure received from Leap Motion service
     * Based on lib/protocol.js protocol function
     */
    export interface FrameData {
        event?: EventData;
        [key: string]: unknown;
    }

    /**
     * Message object for encoding protocol messages
     * Based on lib/protocol.js sendBackground, sendFocused, sendOptimizeHMD functions
     */
    export interface ProtocolMessage {
        background?: boolean;
        focused?: boolean;
        optimizeHMD?: boolean;
        [key: string]: unknown;
    }

    /**
     * Event class for handling Leap Motion events.
     * Based on lib/protocol.js
     */
    export class Event {
        type: string;
        state: Record<string, unknown>;
        constructor(data: EventData);
    }

    /**
     * Protocol interface for handling different protocol versions.
     * Based on lib/protocol.js
     */
    export interface Protocol extends EventEmitter {
        version: number;
        serviceVersion?: string;
        versionLong: string;
        type: 'protocol';
        encode(message: ProtocolMessage): string;
        sendBackground?(connection: BaseConnection, state: boolean): void;
        sendFocused?(connection: BaseConnection, state: boolean): void;
        sendOptimizeHMD?(connection: BaseConnection, state: boolean): void;
    }

    /**
     * JSON Protocol implementation function with EventEmitter methods
     * Based on lib/protocol.js JSONProtocol
     */
    export interface JSONProtocol extends EventEmitter {
        (frameData: FrameData): Event | Frame;
        encode(message: ProtocolMessage): string;
        version: number;
        serviceVersion?: string;
        versionLong: string;
        type: 'protocol';
    }

    /**
     * Protocol selection function
     * Based on lib/protocol.js chooseProtocol
     */
    export function chooseProtocol(header: ProtocolHeader): Protocol;

    /**
     * Create a JSONProtocol instance
     * Based on lib/protocol.js JSONProtocol factory function
     */
    export function JSONProtocolFactory(header: ProtocolHeader): JSONProtocol;

    /**
     * Base connection class for Leap Motion WebSocket connections.
     * Based on lib/connection/base.js
     */
    export class BaseConnection extends EventEmitter {
        static defaultProtocolVersion: number;
        
        opts: {
            host: string;
            scheme: string;
            port: number;
            background: boolean;
            optimizeHMD: boolean;
            requestProtocolVersion: number;
        };
        host: string;
        port: number;
        scheme: string;
        protocolVersionVerified: boolean;
        background: boolean | null;
        optimizeHMD: boolean | null;
        connected: boolean;
        socket?: WebSocket;
        protocol?: Protocol;
        reconnectionTimer?: NodeJS.Timeout;
        focusedState?: boolean;

        constructor(opts?: Partial<BaseConnection['opts']>);
        
        getUrl(): string;
        getScheme(): string;
        getPort(): number;
        setBackground(state: boolean): void;
        setOptimizeHMD(state: boolean): void;
        handleOpen(): void;
        handleClose(code: number, reason: string): void;
        startReconnection(): void;
        stopReconnection(): void;
        disconnect(allowReconnect?: boolean): boolean;
        reconnect(): void;
        handleData(data: string): void;
        connect(): boolean;
        send(data: string): void;
        reportFocus(state: boolean): void;
        setupSocket(): WebSocket;
    }

    /**
     * Browser-specific connection implementation.
     * Based on lib/connection/browser.js
     */
    export class BrowserConnection extends BaseConnection {
        windowVisible?: boolean;
        focusDetectorTimer?: NodeJS.Timeout;

        useSecure(): boolean;
        getScheme(): string;
        getPort(): number;
        setupSocket(): WebSocket;
        startFocusLoop(): void;
        stopFocusLoop(): void;
    }

    /**
     * Node.js-specific connection implementation.
     * Based on lib/connection/node.js
     */
    export class NodeConnection extends BaseConnection {
        setupSocket(): WebSocket;
    }
}
