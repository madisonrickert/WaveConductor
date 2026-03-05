import * as THREE from "three";
import { MathUtils } from "three";
import { EffectComposer, ShaderPass, RenderPass, UnrealBloomPass } from "three-stdlib";

import GPUComputationRenderer, { GPUComputationRendererVariable } from "./gpuComputationRenderer";
import { Sketch } from "@/common/sketch";
import { SettingDef } from "@/common/sketchSettings";
import { loadSettings } from "@/common/sketchSettingsStore";
import { CymaticsAudio } from "./audio";
import { RenderCymaticsShader } from "./renderCymaticsShader";
import { LeapHandController } from "@/common/leap/LeapHandController";

import COMPUTE_CELL_STATE from "./computeCellState.frag";

/** Compute a sane default vertical resolution based on screen size. */
function defaultVerticalResolution(): number {
    return 480;
}

/** Compute a sane default iteration count based on screen size. */
function defaultIterations(): number {
    if (screen.width > 960) return 30;
    if (screen.width > 480) return 20;
    return 15;
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
    static id = "cymatics";

    static settings = {
        verticalResolution: {
            default: defaultVerticalResolution(),
            category: "dev",
            label: "Vertical resolution",
            requiresRestart: true,
            step: 1,
            min: 1,
            max: 1080,
        } satisfies SettingDef<number>,
        iterations: {
            default: defaultIterations(),
            category: "dev",
            label: "Iterations per frame",
            requiresRestart: true,
            step: 1,
            min: 1,
            max: 120,
        } satisfies SettingDef<number>,
    };

    public slowDownAmount = 0;

    private leapHands!: LeapHandController;
    private _handComposer!: EffectComposer;

    protected idleTimeoutSeconds = IDLE_TIMEOUT_SECONDS;

    private mousePressed = false;
    private mousePosition = new THREE.Vector2(0, 0);
    private mousePosition2 = new THREE.Vector2(0, 0);
    /** Leap hand ID holding each center, or null if free */
    private centerHeldByHandId: [string | null, string | null] = [null, null];
    /** The vertical resolution used for this sketch instance (set in init). */
    private verticalRes!: number;
    /** Number of simulation iterations per frame (set in init). */
    private numIterations!: number;

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

    private pixelToNDC(pixelX: number, pixelY: number, out: THREE.Vector2) {
        out.set(pixelX / this.canvas.width * 2 - 1, (1 - pixelY / this.canvas.height) * 2 - 1);
    }

    setMouse(pixelX: number, pixelY: number) {
        this.pixelToNDC(pixelX, pixelY, this.mousePosition);
    }

    setMouse2(pixelX: number, pixelY: number) {
        this.pixelToNDC(pixelX, pixelY, this.mousePosition2);
    }

    public computation!: GPUComputationRenderer;

    public cellStateVariable!: GPUComputationRendererVariable;
    public renderCymaticsPass!: ShaderPass;
    public composer!: EffectComposer;
    public audio!: CymaticsAudio;

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

        const settings = loadSettings("cymatics", Cymatics.settings);
        this.verticalRes = Math.max(1, Math.min(1080, Math.round(settings.verticalResolution)));
        this.numIterations = Math.max(1, Math.min(120, Math.round(settings.iterations)));
        const screenAR = this.canvas.width / this.canvas.height;
        const horizontalRes = Math.round(this.verticalRes * screenAR);
        this.computation = new GPUComputationRenderer(horizontalRes, this.verticalRes, this.renderer);

        const initialTexture = this.computation.createTexture();
        this.cellStateVariable = this.computation.addVariable("cellStateVariable", COMPUTE_CELL_STATE, initialTexture);
        this.cellStateVariable.wrapS = THREE.ClampToEdgeWrapping;
        this.cellStateVariable.wrapT = THREE.ClampToEdgeWrapping;
        // Use linear filtering so the render shader bilinearly interpolates between
        // simulation texels, reducing pixelation at lower resolutions for free.
        // Computation reads are always at exact texel centers so this doesn't affect simulation accuracy.
        this.cellStateVariable.minFilter = THREE.LinearFilter;
        this.cellStateVariable.magFilter = THREE.LinearFilter;
        this.computation.setVariableDependencies(this.cellStateVariable, [this.cellStateVariable]);
        this.cellStateVariable.material.uniforms.iGlobalTime = { value: 0 };
        this.cellStateVariable.material.uniforms.center = { value: new THREE.Vector2(0.5, 0.5) };
        this.cellStateVariable.material.uniforms.center2 = { value: new THREE.Vector2(0.5, 0.5) };
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
        this.leapHands = new LeapHandController({
            canvas: this.canvas,
            renderer: this.renderer,
            getConnectionCallback: () => this.updateLeapConnectionCallback,
            getProtocolVersionCallback: () => this.updateLeapProtocolVersionCallback,
            renderMode: { type: "overlay" },
            handMaterial: new THREE.MeshBasicMaterial({
                color: new THREE.Color(235 / 255, 89 / 255, 56 / 255),
                wireframeLinewidth: 5,
                wireframe: true,
            }),
            onFrame: (hands) => {
                // Release centers whose hands are gone or no longer grabbing
                for (let i = 0; i < 2; i++) {
                    const heldId = this.centerHeldByHandId[i];
                    if (heldId === null) continue;
                    const hand = hands.find(h => h.hand.id === heldId);
                    if (!hand || hand.hand.grabStrength <= 0.5) {
                        this.centerHeldByHandId[i] = null;
                    }
                }

                // For newly grabbing hands not yet assigned, attach to nearest free center
                for (const h of hands) {
                    if (h.hand.grabStrength <= 0.5) continue;
                    if (this.centerHeldByHandId[0] === h.hand.id || this.centerHeldByHandId[1] === h.hand.id) continue;

                    // Convert hand position to sim UV for proximity check
                    this.pixelToNDC(h.canvasPosition.x, h.canvasPosition.y, this.tmpHandNDC);
                    const handUV = this.screenToSimUV(this.tmpHandNDC, this.tmpHandUV);

                    const c1 = this.cellStateVariable.material.uniforms.center.value as THREE.Vector2;
                    const c2 = this.cellStateVariable.material.uniforms.center2.value as THREE.Vector2;
                    const d1 = this.centerHeldByHandId[0] === null ? handUV.distanceTo(c1) : Infinity;
                    const d2 = this.centerHeldByHandId[1] === null ? handUV.distanceTo(c2) : Infinity;

                    if (d1 <= d2) {
                        this.centerHeldByHandId[0] = h.hand.id;
                    } else {
                        this.centerHeldByHandId[1] = h.hand.id;
                    }

                    // Trigger interaction effects on new grab
                    this.slowDownAmount += 1;
                    this.audio.triggerJitter();
                }

                // Update positions of held centers
                for (const h of hands) {
                    if (this.centerHeldByHandId[0] === h.hand.id) {
                        this.setMouse(h.canvasPosition.x, h.canvasPosition.y);
                    }
                    if (this.centerHeldByHandId[1] === h.hand.id) {
                        this.setMouse2(h.canvasPosition.x, h.canvasPosition.y);
                    }
                }

                if (hands.length > 0) {
                    this.markInteraction();
                }
            },
        });

        // Hand mesh bloom rendering (uses LeapHandController's overlay scene/camera)
        this._handComposer = new EffectComposer(this.renderer);
        const handRenderPass = new RenderPass(this.leapHands.handScene!, this.leapHands.handCamera!);
        handRenderPass.clearColor = new THREE.Color(0x000000);
        handRenderPass.clearAlpha = 0;
        this._handComposer.addPass(handRenderPass);
        this._handComposer.addPass(new UnrealBloomPass(
            new THREE.Vector2(this.canvas.width, this.canvas.height),
            1.5, 0.5, 0.0,
        ));
        this._handComposer.addPass(new ShaderPass(new THREE.ShaderMaterial({
            uniforms: { tDiffuse: { value: null } },
            vertexShader: /* glsl */`
                varying vec2 vUv;
                void main() {
                    vUv = uv;
                    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
                }
            `,
            fragmentShader: /* glsl */`
                uniform sampler2D tDiffuse;
                varying vec2 vUv;
                void main() {
                    gl_FragColor = texture2D(tDiffuse, vUv);
                }
            `,
            blending: THREE.AdditiveBlending,
            transparent: true,
        })));
    }

    public animate(_dt: number) {
        const currentTimeMs = performance.now();

        // Check for Leap Motion interaction and reset screensaver timer
        if (this.leapHands.activeHandCount > 0) {
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
        const c1Held = this.centerHeldByHandId[0] !== null;
        const c2Held = this.centerHeldByHandId[1] !== null;
        const interacting = this.mousePressed || c1Held || c2Held;

        if (interacting) {
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

        const center1 = this.cellStateVariable.material.uniforms.center.value as THREE.Vector2;
        const center2 = this.cellStateVariable.material.uniforms.center2.value as THREE.Vector2;
        const wantedCenter1 = this.computeWantedCenter();

        // Update held centers from their hand positions
        if (c1Held) {
            center1.lerp(wantedCenter1, INTERACTION_CENTER_LERP_FACTOR);
        }
        if (c2Held) {
            center2.lerp(this.computeWantedCenter2(), INTERACTION_CENTER_LERP_FACTOR);
        }

        // Free centers mirror the other; if neither held, center1 follows mouse
        if (!c1Held) {
            if (c2Held) {
                this.tmpWantedCenter.set(1 - center2.x, 1 - center2.y);
                center1.lerp(this.tmpWantedCenter, INTERACTION_CENTER_LERP_FACTOR);
            } else {
                center1.lerp(wantedCenter1, INTERACTION_CENTER_LERP_FACTOR);
            }
        }
        if (!c2Held) {
            this.tmpWantedCenter2.set(1 - center1.x, 1 - center1.y);
            center2.lerp(this.tmpWantedCenter2, INTERACTION_CENTER_LERP_FACTOR);
        }

        const centerSpeed = wantedCenter1.distanceTo(center1) * INTERACTION_CENTER_LERP_FACTOR;

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

        const wantedSimulationDt = cycles * Math.PI * 2 / this.numIterations;
        for (let i = 0; i < this.numIterations; i++) {
            this.cellStateVariable.material.uniforms.iGlobalTime.value = this.simulationTime;
            this.computation.compute();
            this.simulationTime += wantedSimulationDt;
        }

        this.renderCymaticsPass.uniforms.skewIntensity.value = skewIntensity;
        this.renderCymaticsPass.uniforms.cellStateVariable.value = this.computation.getCurrentRenderTarget(this.cellStateVariable).texture;
        this.composer.render();

        // Render bloomed hand meshes on top of the cymatics output
        if (this.leapHands.activeHandCount > 0) {
            this.renderer.autoClear = false;
            this._handComposer.render();
            this.renderer.autoClear = true;
        }

        this.slowDownAmount *= 0.95;
    }

    private tmpScreenCoord = new THREE.Vector2();
    private tmpWantedCenter = new THREE.Vector2();
    private tmpWantedCenter2 = new THREE.Vector2();
    private tmpHandNDC = new THREE.Vector2();
    private tmpHandUV = new THREE.Vector2();

    /**
     * Maps mouse NDC position to simulation UV space [0, 1].
     * Uses screen-to-sim aspect ratio correction so the UV mapping matches the render shader.
     */
    private screenToSimUV(mousePos: THREE.Vector2, out: THREE.Vector2): THREE.Vector2 {
        const screenCoord = this.tmpScreenCoord.copy(mousePos).multiplyScalar(0.5);
        const screenAR = this.canvas.width / this.canvas.height;
        const simAR = this.computation.sizeX / this.computation.sizeY;

        out.set(
            screenCoord.x * (screenAR / simAR) + 0.5,
            screenCoord.y + 0.5,
        );

        out.x = MathUtils.clamp(out.x, 0, 1);
        out.y = MathUtils.clamp(out.y, 0, 1);
        return out;
    }

    private computeWantedCenter(): THREE.Vector2 {
        return this.screenToSimUV(this.mousePosition, this.tmpWantedCenter);
    }

    private computeWantedCenter2(): THREE.Vector2 {
        return this.screenToSimUV(this.mousePosition2, this.tmpWantedCenter2);
    }

    resize(width: number, height: number) {
        this.renderCymaticsPass.uniforms.resolution.value.set(width, height);
        this.leapHands?.resize(width, height);
        this._handComposer?.setSize(width, height);
    }

    destroy(): void {
        // Clean up audio resources
        this.audio.dispose();

        // Clean up Leap Motion controller
        this.leapHands.dispose();

        // Clean up hand mesh resources
        this._handComposer.dispose();

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
