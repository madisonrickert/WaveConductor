import * as THREE from "three";
import { RenderPass, EffectComposer } from "three-stdlib";
import { GravityShaderPass } from "./shaders/gravity";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem } from "@/particles";
import { Attractor } from "@/particles/attractor";
import { triangleWaveApprox } from "@/math";
import { loadSettings } from "@/settings/store";
import { SettingDef } from "@/settings/types";
import { BaseSketch } from "@/sketch/BaseSketch";
import { disposeComposer } from "@/sketch/disposeComposer";
import { createAudioGroup, LineSketchAudioGroup } from "./audio";
import { starMaterial } from "@/materials/starMaterial";
import { sampleParticlesFromHeatmap } from "./heatmapSampler";

const LEAP_ATTRACTOR_POWER_ATTACK_SPEED = 0.005;
const LEAP_ATTRACTOR_POWER_DECAY_SPEED = 0.5;

const PARTICLE_SYSTEM_PARAMS = {
    GRAVITY_CONSTANT: 280,
    INERTIAL_DRAG_CONSTANT: 0.53913643334,
    PULLING_DRAG_CONSTANT: 0.93075095702,
    timeStep: 0.016 * 2,
    STATIONARY_CONSTANT: 0.0,
    FADE_DURATION: 3,
    constrainToBox: true,
};

const MOUSE_ATTRACTOR_POWER_DECAY_SPEED = 0.9;
const MOUSE_ATTRACTOR_POWER_DECAY_FLOOR = 2;

export default class LineSketch extends BaseSketch {
    static id = "line";
    static settings = {
        particleDensity: { default: 10, category: "dev", label: "Particle density (per px)", requiresRestart: true } satisfies SettingDef<number>,
        gamma: { default: 1.0, category: "dev", label: "Gamma", requiresRestart: true, step: 0.1 } satisfies SettingDef<number>,
        spawnTemplate: { default: "", category: "dev", label: "Spawn template image", requiresRestart: true, type: "image" } satisfies SettingDef<string>,
    };
    public events = {
        touchstart: (event: TouchEvent) => {
            // Prevent emulated mouse events from occuring
            event.preventDefault();
            const touch = event.touches[0];
            if (!touch) {
                return;
            }
            const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
            let touchY = y;
            // Offset the touchY by its radius so the attractor is above the thumb
            touchY -= 100;

            this.setGravityFocalPoint(x, touchY);
            this.enableMouseAttractor(x, touchY);
            this.markInteraction(); // Reset screensaver timer
        },

        touchmove: (event: TouchEvent) => {
            const touch = event.touches[0];
            if (!touch) {
                return;
            }
            const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
            let touchY = y;
            touchY -= 100;

            this.setGravityFocalPoint(x, touchY);
            this.moveMouseAttractor(x, touchY);
            this.markInteraction(); // Reset screensaver timer
        },

        touchend: (_event: TouchEvent) => {
            this.disableMouseAttractor();
        },

        mousedown: (event: MouseEvent) => {
            if (event.button === 0) {
                const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
                this.setGravityFocalPoint(x, y);
                this.enableMouseAttractor(x, y);
                this.markInteraction(); // Reset screensaver timer
            }
        },

        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.setGravityFocalPoint(x, y);
            this.moveMouseAttractor(x, y);
            this.markInteraction(); // Reset screensaver timer
        },

        mouseup: (event: MouseEvent) => {
            if (event.button === 0) {
                this.disableMouseAttractor();
            }
        },
    };

    public audioGroup!: LineSketchAudioGroup;
    public particles: IParticle[] = [];

    // Three.js & Rendering
    public mouseAttractor: Attractor = new Attractor();
    public leapAttractors: Attractor[] = [];
    private activeAttractors: Attractor[] = [];
    public camera = new THREE.OrthographicCamera(0, 0, 0, 0, 1, 1000);
    public gravityShaderPass = new GravityShaderPass();
    public gravityFocalX = 0;
    public gravityFocalY = 0;
    public scene = new THREE.Scene();
    public pointCloud!: THREE.Points;
    public composer!: EffectComposer;
    public ps!: ParticleSystem;

    /**
     * Returns the Leap-managed attractor at the given index, creating it if necessary.
     * Adds its mesh to the scene if newly created.
     */
    public getLeapAttractor(index: number): Attractor {
        while (this.leapAttractors.length <= index) {
            const attractor = new Attractor();
            this.leapAttractors.push(attractor);
            if (this.scene) {
                this.scene.add(attractor.ringMeshesGroup);
            }
        }
        return this.leapAttractors[index];
    }

    public init() {
        const params = loadSettings("line", LineSketch.settings);

        // Set up audio
        this.audioGroup = createAudioGroup(this.audioContext);

        // Set up camera and scene
        this.resize(this.canvas.width, this.canvas.height);
        this.camera.position.z = 500;

        // Add mouse attractor mesh to scene
        this.scene.add(this.mouseAttractor.ringMeshesGroup);

        const particleCount = Math.round(params.particleDensity * this.canvas.width);

        // Spawn particles — either from a heatmap image or in a default horizontal line
        const initParticles = (particles: IParticle[]) => {
            this.particles = particles;
            this.ps = new ParticleSystem(this.canvas, this.particles, PARTICLE_SYSTEM_PARAMS);
            this.pointCloud = createParticlePoints(this.particles, starMaterial);
            this.scene.add(this.pointCloud);
        };

        if (params.spawnTemplate) {
            // Load the template image and sample particle positions from brightness
            const img = new Image();
            img.onload = () => {
                const particles = sampleParticlesFromHeatmap(img, this.canvas.width, this.canvas.height, particleCount);
                initParticles(particles);
            };
            img.src = params.spawnTemplate;
        } else {
            // Default: evenly space particles across the middle of the screen in a line
            const particles: IParticle[] = [];
            for (let i = 0; i < particleCount; i++) {
                particles.push(createParticle(
                    i / particleCount * this.canvas.width,
                    this.canvas.height / 2 + ((i % 5) - 2) * 2, // Very subtle sawtooth wave
                ));
            }
            initParticles(particles);
        }

        // Set up postprocessing composer and passes
        this.composer = new EffectComposer(this.renderer);
        this.composer.addPass(new RenderPass(this.scene, this.camera));
        this.gravityShaderPass.uniforms.iResolution.value = new THREE.Vector2(this.canvas.width, this.canvas.height);
        this.gravityShaderPass.uniforms.gamma.value = params.gamma;
        this.gravityShaderPass.renderToScreen = true;
        this.composer.addPass(this.gravityShaderPass);

        // Set up Leap Motion controller
        this.leapHands = this.createLeapController({
            renderMode: { type: "in-scene", scene: this.scene },
            onFrame: (hands) => {
                hands.forEach(({ hand, index, canvasPosition }) => {
                    if (index === 0) {
                        this.setGravityFocalPoint(canvasPosition.x, canvasPosition.y);
                    }
                    const attractor = this.getLeapAttractor(index);
                    attractor.x = canvasPosition.x;
                    attractor.y = canvasPosition.y;
                    const position = hand.palmPosition;
                    if (hand.grabStrength === 0) {
                        attractor.power *= LEAP_ATTRACTOR_POWER_DECAY_SPEED;
                    } else {
                        const grabComponent = Math.pow(hand.grabStrength, 1.5);
                        const depthModulator = Math.pow(5, (-position[2] + 350) / 160);
                        const wantedPower = grabComponent * depthModulator;
                        attractor.power = attractor.power * (1 - LEAP_ATTRACTOR_POWER_ATTACK_SPEED) + wantedPower * LEAP_ATTRACTOR_POWER_ATTACK_SPEED;
                    }
                });
                for (let i = hands.length; i < this.leapAttractors.length; i++) {
                    this.leapAttractors[i].power = 0;
                }
            },
        });
    }

    public animate(millisElapsed: number) {
        // Attractor ring animation and mouse power decay must run even when idle
        this.mouseAttractor.animate(millisElapsed);
        for (const attractor of this.leapAttractors) {
            attractor.animate(millisElapsed);
        }
        if (this.mouseAttractor.power > 0) {
            this.mouseAttractor.power =
                MOUSE_ATTRACTOR_POWER_DECAY_FLOOR +
                (this.mouseAttractor.power - MOUSE_ATTRACTOR_POWER_DECAY_FLOOR) * MOUSE_ATTRACTOR_POWER_DECAY_SPEED;
        }
        super.animate(millisElapsed);
    }

    protected step(now: number = performance.now()): void {
        // Guard: particles may not be initialized yet if spawn template is loading
        if (!this.ps) return;

        // Step particles with all active attractors
        this.activeAttractors.length = 0; // clear without reallocating
        if (this.mouseAttractor.power !== 0) {
            this.activeAttractors.push(this.mouseAttractor);
        }
        for (const attractor of this.leapAttractors) {
            if (attractor.power !== 0) {
                this.activeAttractors.push(attractor);
            }
        }
        this.ps.stepParticles(this.activeAttractors, this.pointCloud);

        // --- Audio Feedback ---
        const {
            groupedUpness,
            normalizedVarianceLength,
            flatRatio,
            normalizedEntropy
        } = computeStats(this.ps);

        const sourceLfoFreq = this.audioGroup.sourceLfo.frequency;
        const currentAudioTime = this.audioContext.currentTime;
        sourceLfoFreq.cancelScheduledValues(currentAudioTime);
        sourceLfoFreq.setTargetAtTime(flatRatio, currentAudioTime, 0.016);
        if (normalizedEntropy !== 0) {
            this.audioGroup.setFrequency(222 / normalizedEntropy);
        }
        const noiseFreq = 2000 * normalizedVarianceLength;
        this.audioGroup.setNoiseFrequency(noiseFreq);
        this.audioGroup.setVolume(Math.max(groupedUpness - 0.05, 0) * 5.);

        // --- Shader Uniforms ---
        this.gravityShaderPass.uniforms.iGlobalTime.value = now / 1000;
        this.gravityShaderPass.uniforms.G.value = triangleWaveApprox(now / 5000) * (groupedUpness + 0.50) * 15000;
        this.gravityShaderPass.uniforms.iMouseFactor.value = (1 / 15) / (groupedUpness + 1);
        this.gravityShaderPass.uniforms.iMouse.value.set(
            this.gravityFocalX,
            this.renderer.domElement.height - this.gravityFocalY
        );

        // --- Render ---
        this.composer.render();
    }

    public resize(width: number, height: number) {
        this.camera.right = width;
        this.camera.bottom = height;
        this.camera.updateProjectionMatrix();
        this.gravityShaderPass.uniforms.iResolution.value = new THREE.Vector2(width, height);
        this.composer?.setSize(width, height);
    }

    public destroy(): void {
        super.destroy();
        this.audioGroup.dispose();

        // Clear scene
        this.particles.length = 0;
        this.scene.clear();
        
        disposeComposer(this.composer);

        // Dispose point cloud (may not exist if image was still loading)
        this.pointCloud?.geometry.dispose();
    }

    public setGravityFocalPoint(x: number, y: number) {
        this.gravityFocalX = x;
        this.gravityFocalY = y;
    }

    private hasActiveAttractors(): boolean {
        if (this.mouseAttractor.power > MOUSE_ATTRACTOR_POWER_DECAY_FLOOR + 1e-2) {
            return true;
        }
        return this.leapAttractors.some((attractor) => attractor.power > 1e-2);
    }

    // --- Attractor Controls ---
    private enableMouseAttractor(x: number, y: number) {
        this.mouseAttractor.x = x;
        this.mouseAttractor.y = y;
        this.mouseAttractor.power = 10;
    }

    private moveMouseAttractor(x: number, y: number) {
        this.mouseAttractor.x = x;
        this.mouseAttractor.y = y;
    }

    private disableMouseAttractor() {
        this.mouseAttractor.power = 0;
    }

    protected isReadyToSleep(): boolean {
        return !this.hasActiveAttractors();
    }
}
