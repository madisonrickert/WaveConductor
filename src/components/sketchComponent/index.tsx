import $ from "jquery";
import React from "react";
import { Link } from "react-router";
import * as THREE from "three";
import classnames from "classnames";

import { ISketch, SketchAudioContext, SketchConstructor, UI_EVENTS, UIEventReciever } from "@/sketch";
import { VolumeButton } from "@/components/volumeButton";
import { HandData, HandOverlay } from "@/components/HandOverlay";
import { ScreenSaver } from "@/components/screenSaver";

import "./sketchComponent.scss";

const $window = $(window);

export interface ISketchComponentProps extends React.DOMAttributes<HTMLDivElement> {
    errorElement?: React.JSX.Element;
    sketchClass: SketchConstructor;
}

export interface SketchSuccess {
    type: "success";
    sketch: ISketch;
}

export interface SketchError {
    type: "error";
    error: Error;
}

export interface SketchLoading {
    type: "loading";
}

export type SketchStatus = SketchSuccess | SketchError | SketchLoading;

interface SketchSuccessComponentProps {
    sketch: ISketch;
}
/**
 * SketchSuccessComponent is responsible for:
 * - running init code on sketch
 * - firing resize events
 * - attaching ui event listeners
 * - keeping focus on the canvas
 */
class SketchSuccessComponent extends React.Component<SketchSuccessComponentProps, {frameCount: number}> {
    private frameId?: number;
    private lastTimestamp = 0;
    constructor(props: SketchSuccessComponentProps) {
        super(props);
        this.state = {
            frameCount: props.sketch.frameCount,
        }
    };

    componentDidMount() {
        this.updateRendererCanvasToMatchParent(this.props.sketch.renderer);
        $window.on('resize', this.handleWindowResize);

        // canvas setup
        const $canvas = $(this.props.sketch.renderer.domElement);
        $canvas.attr("tabindex", 1);
        this.attachUIEvents($canvas);
        // prevent scrolling the viewport
        // $canvas.on("touchmove", (event) => {
        //     event.preventDefault();
        // });

        // TODO handle errors here
        this.props.sketch.init();
        this.frameId = requestAnimationFrame(this.loop);
    }

    render() {
        const { sketch } = this.props;
        return (
            <div className="sketch-elements">
                {sketch.render?.()}
                {sketch.elements?.map((el, idx) => React.cloneElement(el, { key: idx }))}
            </div>
        );
    }

    componentWillUnmount() {
        if (this.props.sketch.destroy) {
            this.props.sketch.destroy();
        }
        if (this.frameId != null) {
            cancelAnimationFrame(this.frameId);
        }
        this.props.sketch.renderer.dispose();
        $window.off("resize", this.handleWindowResize);

        const $canvas = $(this.props.sketch.canvas);
        this.removeUIEvents($canvas);
    }

    private attachUIEvents($target: JQuery<HTMLElement>) {
        const events = this.props.sketch.events as UIEventReciever;
        Object.entries(events).forEach(([eventName, callback]) => {
            if (callback) {
                $target.on(eventName, callback);
            }
        });
    }

    private removeUIEvents($target: JQuery<HTMLElement>) {
        const events = this.props.sketch.events as UIEventReciever;
        (Object.keys(UI_EVENTS) as Array<keyof typeof UI_EVENTS>).forEach((eventName) => {
            const callback = events[eventName];
            if (callback != null) {
                $target.off(eventName, callback);
            }
        });
    }

    private loop = (timestamp: number) => {
        const millisElapsed = timestamp - this.lastTimestamp;
        this.lastTimestamp = timestamp;
        this.props.sketch.frameCount++;
        this.props.sketch.timeElapsed = timestamp;
        try {
            this.props.sketch.animate(millisElapsed);
        } catch (e) {
            console.error(e);
        }

        // force new render()
        this.setState({
            frameCount: this.props.sketch.frameCount,
        });
        this.frameId = requestAnimationFrame(this.loop);
    }

    private handleWindowResize = () => {
        const { renderer } = this.props.sketch;
        this.updateRendererCanvasToMatchParent(renderer);
        if (this.props.sketch.resize != null) {
            this.props.sketch.resize(renderer.domElement.width, renderer.domElement.height);
        }
    }

    private updateRendererCanvasToMatchParent(renderer: THREE.WebGLRenderer) {
        const parent = renderer.domElement.parentElement;
        if (parent != null) {
            renderer.setSize(parent.clientWidth, parent.clientHeight);
        }
    }
}

export interface ISketchComponentState {
    status: SketchStatus;
    volumeEnabled: boolean;
    handData: HandData[];
    shouldShowScreenSaver: boolean;
}

export class SketchComponent extends React.Component<ISketchComponentProps, ISketchComponentState> {
    public state = {
        status: { type: "loading" } as SketchStatus,
        volumeEnabled: JSON.parse(window.localStorage.getItem("sketch-volumeEnabled") || "true"),
        handData: [] as HandData[],
        shouldShowScreenSaver: false,
    };

    private renderer?: THREE.WebGLRenderer;
    private audioContext?: SketchAudioContext;
    private userVolume?: GainNode;

    private handleContainerRef = (ref: HTMLDivElement | null) => {
        if (ref != null) {
            try {
                // create dependencies, setup sketch, and move to success state
                // we are responsible for live-updating the global user volume.
                const audioContext = this.audioContext = new AudioContext() as SketchAudioContext;
                THREE.AudioContext.setContext(audioContext);
                this.userVolume = audioContext.createGain();
                // Set initial volume based on persisted state.
                this.userVolume.gain.value = this.state.volumeEnabled ? 1 : 0;
                this.userVolume.connect(audioContext.destination);
                const audioContextGain = audioContext.gain = audioContext.createGain();
                audioContextGain.connect(this.userVolume);
                document.addEventListener("visibilitychange", this.handleVisibilityChange);

                if (!this.renderer) {
                    this.renderer = new THREE.WebGLRenderer({ alpha: true, preserveDrawingBuffer: true, antialias: true });
                    ref.appendChild(this.renderer.domElement);
                }

                const sketchClassInstance = new this.props.sketchClass(this.renderer, this.audioContext);
                sketchClassInstance.updateScreenSaverCallback = this.updateScreenSaverCallback;
                sketchClassInstance.updateHandDataCallback = this.handleHandDataUpdate;
                this.setState({ status: { type: "success", sketch: sketchClassInstance } });
            } catch (e) {
                this.setState({ status: { type: "error", error: e instanceof Error ? e : new Error(String(e)) }});
                console.error(e);
            }
        } else {
            document.removeEventListener("visibilitychange", this.handleVisibilityChange);
            if (this.audioContext != null) {
                this.audioContext.close();
            }
        }
    };

    private handleHandDataUpdate = (handData: HandData[]) => {
        this.setState({ handData });
    };

    private updateScreenSaverCallback = (shouldShow: boolean) => {
        this.setState({ shouldShowScreenSaver: shouldShow });
    };

    componentDidUpdate(prevProps: Readonly<ISketchComponentProps>, prevState: Readonly<ISketchComponentState & { handData: HandData[] }>) {
        if (prevState.volumeEnabled !== this.state.volumeEnabled && this.audioContext && this.userVolume) {
            const volume = this.state.volumeEnabled ? 1 : 0;
            this.userVolume.gain.value = volume;
            if (this.state.volumeEnabled && this.audioContext.state === "suspended") {
                this.audioContext.resume();
            } else if (!this.state.volumeEnabled && this.audioContext.state === "running") {
                this.audioContext.suspend();
            }
        }
    }

    public render() {

        const { sketchClass: _sketchClass, ...containerProps } = this.props;
        const className = classnames("sketch-component", this.state.status.type);
        return (
            <div {...containerProps} id={this.props.sketchClass.id} className={className} ref={this.handleContainerRef}>
                <div style={{ position: "relative" }}>
                    {this.renderSketchOrStatus()}
                </div>
                <ScreenSaver shouldShow={this.state.shouldShowScreenSaver} />
                <VolumeButton
                    volumeEnabled={this.state.volumeEnabled}
                    onClick={this.handleVolumeButtonClick}
                />
            </div>
        );
    }

    private renderSketchOrStatus() {
        const { status } = this.state;
        if (status.type === "success") {
            return (
                <>
                    <SketchSuccessComponent key={this.props.sketchClass.id} sketch={status.sketch} />
                    <HandOverlay hands={this.state.handData} />
                </>
            );
        } else if (status.type === "error") {
            const errorElement = this.props.errorElement || this.renderDefaultErrorElement(status.error.message);
            return errorElement;
        } else if (status.type === "loading") {
            return null;
        }
    }

    private renderDefaultErrorElement(message: string) {
        return (
            <p className="sketch-error">
                Oops - something went wrong! Make sure you're using Chrome, or are on your desktop.
                <pre>{message}</pre>
                <p><Link className="back" to="/">Back</Link></p>
            </p>
        );
    }

    private handleVolumeButtonClick = () => {
        const volumeEnabled = !this.state.volumeEnabled;
        this.setState({ volumeEnabled });
        window.localStorage.setItem("sketch-volumeEnabled", JSON.stringify(volumeEnabled));
    }

    private handleVisibilityChange = () => {
        if (this.audioContext != null) {
            if (document.hidden) {
                this.audioContext.suspend();
            } else if (this.state.volumeEnabled) {
                this.audioContext.resume();
            }
        }
    }
}
