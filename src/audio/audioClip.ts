export interface AudioClipOptions {
    context: AudioContext;
    srcs: string[];
    autoplay?: boolean;
    loop?: boolean;
    volume?: number;
}

const DEFAULT_OPTIONS = {
    volume: 1,
    autoplay: false,
    loop: false,
};

const MIME_TYPES: Record<string, string> = {
    mp3: "audio/mpeg",
    ogg: "audio/ogg",
    wav: "audio/wav",
    webm: "audio/webm",
    flac: "audio/flac",
    aac: "audio/aac",
};

export class AudioClip {
    private element: HTMLMediaElement;
    private node: MediaElementAudioSourceNode;
    constructor(options: AudioClipOptions) {
        const { autoplay, loop, volume, srcs } = { ...DEFAULT_OPTIONS, ...options };
        this.element = document.createElement("audio");
        // Don't use the HTML autoplay attribute — on some mobile browsers (e.g.
        // Firefox iOS / WKWebView) it can bypass the Web Audio graph and play
        // through the native output before the AudioContext is running.
        this.element.loop = loop;
        this.element.volume = volume;
        this.element.preload = "auto";
        for (const srcUrl of srcs) {
            const extension = srcUrl.split(".").pop()?.toLowerCase() ?? "";
            const source = document.createElement("source");
            source.src = srcUrl;
            source.type = MIME_TYPES[extension] ?? `audio/${extension}`;
            this.element.appendChild(source);
        }

        this.element.style.display = "none";
        document.body.appendChild(this.element);

        this.node = options.context.createMediaElementSource(this.element);

        // Start playback only once the AudioContext is running, ensuring audio
        // is routed exclusively through the Web Audio graph.
        if (autoplay) {
            const tryPlay = () => {
                if (options.context.state === "running" && this.element.paused) {
                    this.element.play().catch(() => {});
                }
            };
            tryPlay();
            options.context.addEventListener("statechange", tryPlay);
        }
    }

    get volume() {
        return this.element.volume;
    }

    set volume(v: number) {
        this.element.volume = v;
    }

    get playbackRate() {
        return this.element.playbackRate;
    }

    set playbackRate(r: number) {
        this.element.playbackRate = r;
    }

    getNode() {
        return this.node;
    }

    play() {
        this.element.currentTime = 0;
        return this.element.play();
    }

    dispose() {
        this.element.pause();
        this.element.currentTime = 0;
        this.node.disconnect();
        this.element.remove();
    }
}
