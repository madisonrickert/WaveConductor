import { Controller } from "leapjs";
import Leap from "leapjs";
import { mapLeapToThreePosition } from "@/common/leap/util";
import * as THREE from "three";
import { MathUtils } from "three";
import { EffectComposer, ShaderPass/*, RenderPass */} from "three-stdlib";

import GPUComputationRenderer, { GPUComputationRendererVariable } from "./gpuComputationRenderer";
import { mirroredRepeat } from "@/common/math";
import { Sketch } from "@/common/sketch";
import { CymaticsAudio } from "./audio";
import { RenderCymaticsShader } from "./renderCymaticsShader";
import { HandData } from "@/components/handOverlay";

import COMPUTE_CELL_STATE from "./computeCellState.frag";

enum Quality {
    Low,
    Medium,
    High,
}

function computeQuality(): Quality {
    if (screen.width > 960) return Quality.High;
    if (screen.width > 480) return Quality.Medium;
    return Quality.Low;
}

// an integer makes perfect standing waves. the 0.002 means that the wave will oscillate very slightly per frame; 500 frames per oscillation period
const DEFAULT_NUM_CYCLES = 1.002;
const MINIMUM_ACTIVE_RADIUS = 0.1;
const MINIMUM_ACTIVE_RADIUS_INTERACTING = 0.5;
const TARGET_ACTIVE_RADIUS_INTERACTING = 7.5;
const ACTIVE_RADIUS_INTERACTING_GROW_FACTOR = 0.01;
const ACTIVE_RADIUS_IDLE_DECAY_FACTOR = 0.005;

const IDLE_TIMEOUT_SECONDS = 10;

const INTERACTION_CENTER_LERP_FACTOR = 0.01;

export default class Cymatics extends Sketch {
    public slowDownAmount = 0;
    public handData: HandData[] = [];

    protected idleTimeoutSeconds = IDLE_TIMEOUT_SECONDS;

    private mousePressed = false;
    private mousePosition = new THREE.Vector2(0, 0);
    private quality: Quality = computeQuality();

    public events = {
        touchstart: (event: TouchEvent) => {
            // prevent emulated mouse events from occuring
            event.preventDefault();
            const touch = event.touches[0];
            if (touch) {
                const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
                this.startInteraction(x, y);
            }
            this.markInteraction(); // Reset screensaver timer
        },

        touchmove: (event: TouchEvent) => {
            const touch = event.touches[0];
            if (touch) {
                const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
                this.setMouse(x, y);
            }
            this.markInteraction(); // Reset screensaver timer
        },

        touchend: (_event: TouchEvent) => {
            this.mousePressed = false;
        },

        mousedown: (event: MouseEvent) => {
            if (event.button === 0) {
                const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
                this.startInteraction(x, y);
                this.markInteraction(); // Reset screensaver timer
            }
        },

        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.setMouse(x, y);
            this.markInteraction(); // Reset screensaver timer
        },

        mouseup: (_event: MouseEvent) => {
            this.mousePressed = false;
        },
    };

    startInteraction(pixelX: number, pixelY: number) {
        this.setMouse(pixelX, pixelY);
        this.mousePressed = true;
        this.slowDownAmount += 1;
        this.audio.triggerJitter();
        this.markInteraction();
    }

    setMouse(pixelX: number, pixelY: number) {
        this.mousePosition.set(pixelX / this.canvas.width * 2 - 1, (1 - pixelY / this.canvas.height) * 2 - 1);
    }

    static id = "cymatics";

    public computation!: GPUComputationRenderer;

    public cellStateVariable!: GPUComputationRendererVariable;
    public renderCymaticsPass!: ShaderPass;
    public composer!: EffectComposer;
    public audio!: CymaticsAudio;

    public leapController!: Controller;

    public simulationTime = 0;
    public numCycles = DEFAULT_NUM_CYCLES;
    
    public get activeRadius() {
        return this.cellStateVariable.material.uniforms.activeRadius.value;
    }

    public set activeRadius(t: number) {
        this.cellStateVariable.material.uniforms.activeRadius.value = t;
    }

    public init() {
        this.renderer.setClearColor(0xfcfcfc);
        this.renderer.clear();
        switch(this.quality) {
            case Quality.High:
                this.computation = new GPUComputationRenderer(1024, 1024, this.renderer);
                break;
            case Quality.Medium:
                this.computation = new GPUComputationRenderer(512, 512, this.renderer);
                break;
            default:
                this.computation = new GPUComputationRenderer(256, 256, this.renderer);
                break;
        }

        const initialTexture = this.computation.createTexture();
        this.cellStateVariable = this.computation.addVariable("cellStateVariable", COMPUTE_CELL_STATE, initialTexture);
        this.cellStateVariable.wrapS = THREE.MirroredRepeatWrapping;
        this.cellStateVariable.wrapT = THREE.MirroredRepeatWrapping;
        this.computation.setVariableDependencies(this.cellStateVariable, [this.cellStateVariable]);
        this.cellStateVariable.material.uniforms.iGlobalTime = { value: 0 };
        this.cellStateVariable.material.uniforms.center = { value: new THREE.Vector2(0.5, 0.5) };
        this.cellStateVariable.material.uniforms.activeRadius = { value: MINIMUM_ACTIVE_RADIUS };
        const computationInitError = this.computation.init();
        if (computationInitError != null) {
            console.error(computationInitError);
            throw computationInitError;
        }

        this.composer = new EffectComposer(this.renderer);
        this.renderCymaticsPass = new ShaderPass(RenderCymaticsShader);
        this.renderCymaticsPass.renderToScreen = true;
        this.renderCymaticsPass.uniforms.resolution.value.set(this.canvas.width, this.canvas.height);
        this.renderCymaticsPass.uniforms.cellStateResolution.value.set(this.computation.sizeX, this.computation.sizeY);
        this.composer.addPass(this.renderCymaticsPass);
        this.audio = new CymaticsAudio(this.audioContext);

        // Leap Motion setup
        this.leapController = new Leap.Controller()
            .connect()
            .on('frame', this.handleLeapFrame);
    }

    public animate(_dt: number) {
        const currentTimeMs = performance.now();

        // Check for Leap Motion interaction and reset screensaver timer
        if (this.handData.length > 0) {
            this.markInteraction(currentTimeMs);
        }

        if (!this.isIdle) {
            this.animateSimulation();
        }

        this.updateIdleState(currentTimeMs);
    }

    /**
     * Runs each frame that the simulation is active
     */
    private animateSimulation(): void {
        if (this.mousePressed) {
            this.numCycles += .0003 + (this.numCycles - DEFAULT_NUM_CYCLES) * 0.0008;
            if (this.activeRadius < MINIMUM_ACTIVE_RADIUS_INTERACTING) {
                this.activeRadius = MINIMUM_ACTIVE_RADIUS_INTERACTING;
            }
            this.activeRadius = MathUtils.lerp(
                this.activeRadius,
                TARGET_ACTIVE_RADIUS_INTERACTING,
                ACTIVE_RADIUS_INTERACTING_GROW_FACTOR
            );
        } else {
            this.activeRadius = MathUtils.lerp(
                this.activeRadius,
                MINIMUM_ACTIVE_RADIUS,
                ACTIVE_RADIUS_IDLE_DECAY_FACTOR
            );
            this.numCycles = this.numCycles * 0.95 + DEFAULT_NUM_CYCLES * 0.05;
        }

        const wantedCenter = this.computeWantedCenter();
        // how fast the center's moving; max is about 0.06
        const centerSpeed = wantedCenter.distanceTo(this.cellStateVariable.material.uniforms.center.value) * INTERACTION_CENTER_LERP_FACTOR;
        this.cellStateVariable.material.uniforms.center.value.lerp(wantedCenter, INTERACTION_CENTER_LERP_FACTOR);

        const skewIntensity = Math.pow(Math.max(0, (this.numCycles - DEFAULT_NUM_CYCLES) / 2. - 0.5), 2);

        // grows louder as there's more active radius, and also when it moves faster
        const blubVolume = THREE.MathUtils.clamp(Math.pow(THREE.MathUtils.mapLinear(this.activeRadius, MINIMUM_ACTIVE_RADIUS, 1.0, 0.05, 1), 2), 0, 1) * 0.5
                    + Math.abs(this.numCycles - DEFAULT_NUM_CYCLES) * 0.25
                    - skewIntensity
                    + THREE.MathUtils.mapLinear(centerSpeed, 0, 0.005, 0, 1) * THREE.MathUtils.mapLinear(this.activeRadius, MINIMUM_ACTIVE_RADIUS, 1.0, 0.12, 1) * 0.4;
        this.audio.setBlubVolume(blubVolume);
        // play slowly when there's no movement, play faster when there's a lot of movement
        const playbackRate = Math.pow(2, THREE.MathUtils.mapLinear(centerSpeed, 0, 0.005, -0.25, 1.5)) + THREE.MathUtils.mapLinear(this.numCycles, DEFAULT_NUM_CYCLES, 2, 0., 4.);
        this.audio.setBlubPlaybackRate(playbackRate);

        this.audio.setOscVolume(THREE.MathUtils.clamp(THREE.MathUtils.smoothstep(this.numCycles, DEFAULT_NUM_CYCLES, DEFAULT_NUM_CYCLES * 1.1) * 0.5, 0, 1));
        const cycles = (this.numCycles) / (1 + this.slowDownAmount * 3);
        const frequencyScalar = cycles / DEFAULT_NUM_CYCLES;
        this.audio.setOscFrequencyScalar(frequencyScalar);

        let numIterations;
        switch(this.quality) {
            case Quality.High:
                numIterations = 30;
                break;
            case Quality.Medium:
                numIterations = 20;
                break;
            default:
                numIterations = 15;
                break;
        }
        const wantedSimulationDt = cycles * Math.PI * 2 / numIterations;
        for (let i = 0; i < numIterations; i++) {
            this.cellStateVariable.material.uniforms.iGlobalTime.value = this.simulationTime;
            this.computation.compute();
            this.simulationTime += wantedSimulationDt;
        }

        this.renderCymaticsPass.uniforms.skewIntensity.value = skewIntensity;
        this.renderCymaticsPass.uniforms.cellStateVariable.value = this.computation.getCurrentRenderTarget(this.cellStateVariable).texture;
        this.composer.render();

        this.slowDownAmount *= 0.95;
    }

    private handleLeapFrame = (frame: Leap.Frame) => {
        const validHands = frame.hands.filter((hand) => hand.valid);
        const newHands = validHands.flatMap((hand, idx): HandData[] => {
            const finger = hand.indexFinger;
            if (!finger) return [];
            const position = finger.bones[3].center();
            const { x, y } = mapLeapToThreePosition(this.canvas, position);
            return [{ index: idx, position: { x, y }, pinched: !finger.extended }];
        });
        const oldHandCount = this.handData.length;
        this.handData = newHands;
        if (this.updateHandDataCallback) {
            this.updateHandDataCallback(this.handData);
        }
        if (newHands.length === 0 && oldHandCount > 0) {
            this.mousePressed = false;
        }

        if (newHands.length === 0) {
            return;
        }

        // For now, only one hand controls position
        const primaryHand = newHands[0];
        this.setMouse(primaryHand.position.x, primaryHand.position.y);

        // Either hand can pinch or squeeze to trigger the effect
        const isPinched = newHands.some((hand) => hand.pinched);
        if (isPinched) {
            if (!this.mousePressed) {
                this.mousePressed = true;
                this.slowDownAmount += 1;
                this.audio.triggerJitter();
            }
        } else {
            this.mousePressed = false;
        }

        this.markInteraction();
    }

    private tmpScreenCoord = new THREE.Vector2();
    private tmpNormCoord = new THREE.Vector2();
    private tmpUv = new THREE.Vector2();
    private tmpWantedCenter = new THREE.Vector2();

    /**
     * Computes the simulation’s focal point based on the current aspect ratio and mouse/hand position.
     *
     * The cymatics shader splits the viewport into mirrored regions so the visual pattern
     * repeats across wide and tall screens. This helper replicates the fragment shader’s logic
     * in TypeScript so we can smoothly lerp the `center` uniform on the CPU without reallocating
     * vectors each frame.
     *
     * @returns A mutable reference to the cached `Vector2` containing the desired center (range [0, 1]).
     */
    private computeWantedCenter(): THREE.Vector2 {
        // clone-without-alloc: screen-space position in [-1, 1]^2 scaled down to the quadrant size
        const screenCoord = this.tmpScreenCoord.copy(this.mousePosition).multiplyScalar(0.5);

        if (1 / this.aspectRatio > 1.0) {
            // Widescreen layout: split into left/right halves.
            // tmpNormCoord holds the stretch factor, tmpUv accumulates the offset.
            const uv = this.tmpUv
                .copy(screenCoord)
                .multiply(this.tmpNormCoord.set(1 / this.aspectRatio, 1))
                .add(this.tmpNormCoord.set(1, 0.5));
            this.tmpWantedCenter.set(mirroredRepeat(uv.x), mirroredRepeat(uv.y));
        } else {
            // Tall layout: split into top/bottom halves using the reciprocal aspect.
            const uv = this.tmpUv
                .copy(screenCoord)
                .multiply(this.tmpNormCoord.set(1, this.aspectRatio))
                .add(this.tmpNormCoord.set(0.5, 1));
            this.tmpWantedCenter.set(mirroredRepeat(uv.x), mirroredRepeat(uv.y));
        }

        return this.tmpWantedCenter;
    }

    resize(width: number, height: number) {
        this.renderCymaticsPass.uniforms.resolution.value.set(width, height);
    }

    destroy(): void {
        // Clean up audio resources
        this.audio.dispose();

        // Clean up Leap Motion controller
        this.leapController
            .removeListener('frame', this.handleLeapFrame)
            .disconnect();

        // Clean up Three.js resources
        while(this.composer.passes.length > 0) {
            this.composer.passes[0].dispose();
            this.composer.removePass(this.composer.passes[0]);
        }
        this.composer.dispose();
        this.computation.dispose();
    }

    protected isReadyToSleep(): boolean {
        return this.activeRadius <= MINIMUM_ACTIVE_RADIUS + 1e-2;
    }
}
