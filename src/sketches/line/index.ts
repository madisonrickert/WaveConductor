import * as THREE from "three";
import { RenderPass, EffectComposer } from "three-stdlib";
import { GravityShaderPass } from "@/common/shaders/gravity";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem } from "@/common/particleSystem";
import { Attractor } from "@/common/particleSystem/attractor";
import { triangleWaveApprox } from "@/common/math";
import { getQueryParams } from "@/common/queryParams";
import { ISketch } from "@/sketch";
import { createAudioGroup, LineSketchAudioGroup } from "./audio";
import { starMaterial } from "@/common/materials/starMaterial";
import { LeapAttractorController } from "./LeapAttractorController";

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

const SCREEN_SAVER_TIMEOUT_SECONDS = 30;
const MINIMUM_SLEEP_TIMEOUT_SECONDS = 30;

interface LineSketchParams extends Record<string, unknown> {
    p?: number;
    gamma?: number;
}

export default class LineSketch extends ISketch {
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

    // TODO move into core sketch
    private lastInteractionTimestampMs: number = performance.now();
    private isIdle: boolean = false;

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
    public leapAttractorController!: LeapAttractorController;
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
        const params: LineSketchParams = getQueryParams<LineSketchParams>({
            parseNumbers: true
        });

        // Set up audio
        this.audioGroup = createAudioGroup(this.audioContext);

        // Set up camera and scene
        this.resize(this.canvas.width, this.canvas.height);
        this.camera.position.z = 500;

        // Add mouse attractor mesh to scene
        this.scene.add(this.mouseAttractor.ringMeshesGroup);

        // Determine number of particles (query param or screen size)
        const particleCount = params.p || screen.width * 10;
        
        // Evenly space particles across the middle of the screen in a line
        for (let i = 0; i < particleCount; i++) {
            this.particles.push(createParticle(
                i / particleCount * this.canvas.width,
                this.canvas.height / 2 + ((i % 5) - 2) * 2, // Very subtle sawtooth wave
            ));
        }

        // Set up particle system and points
        this.ps = new ParticleSystem(
            this.canvas,
            this.particles,
            PARTICLE_SYSTEM_PARAMS,
        );
        this.pointCloud = createParticlePoints(this.particles, starMaterial);
        this.scene.add(this.pointCloud);

        // Set up postprocessing composer and passes
        this.composer = new EffectComposer(this.renderer);
        this.composer.addPass(new RenderPass(this.scene, this.camera));
        this.gravityShaderPass.uniforms.iResolution.value = new THREE.Vector2(this.canvas.width, this.canvas.height);
        if (params.gamma) {
            this.gravityShaderPass.uniforms.gamma.value = params.gamma;
        }
        this.gravityShaderPass.renderToScreen = true;
        this.composer.addPass(this.gravityShaderPass);

        // Set up Leap Motion controller
        this.leapAttractorController = new LeapAttractorController(this);
    }

    public animate(_millisElapsed: number) {
        const currentTimeMs = performance.now();

        // Animate all attractors
        this.mouseAttractor.animate(_millisElapsed);
        for (const attractor of this.leapAttractors) {
            attractor.animate(_millisElapsed);
        }

        // Check for Leap Motion interaction and reset interaction timer
        if (this.leapAttractorController.hasActiveInteraction()) {
            this.markInteraction(currentTimeMs);
        }

        if (!this.isIdle) {
            this.animateSimulation(currentTimeMs);
        }

        // --- Update attractor power ---
        if (this.mouseAttractor.power > 0) {
            this.mouseAttractor.power =
                MOUSE_ATTRACTOR_POWER_DECAY_FLOOR +
                (this.mouseAttractor.power - MOUSE_ATTRACTOR_POWER_DECAY_FLOOR) * MOUSE_ATTRACTOR_POWER_DECAY_SPEED;
        }

        // --- Sleep logic ---
        const secondsSinceInteraction = (currentTimeMs - this.lastInteractionTimestampMs) / 1000;
        this.isIdle = secondsSinceInteraction >= MINIMUM_SLEEP_TIMEOUT_SECONDS && !this.hasActiveAttractors();

        // --- Screen Saver Logic ---
        if (this.updateScreenSaverCallback) {
            const showScreenSaver = secondsSinceInteraction >= SCREEN_SAVER_TIMEOUT_SECONDS;
            this.updateScreenSaverCallback(showScreenSaver);
        }
    }

    private animateSimulation(now: number = performance.now()): void {
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
        // Clean up audio resources
        this.audioGroup.dispose();

        // Detach Leap Motion controller
        this.leapAttractorController.dispose();

        // Clear scene
        this.particles.length = 0;
        this.scene.clear();
        
        // Dispose of Three.js resources
        while(this.composer.passes.length > 0) {
            this.composer.passes[0].dispose();
            this.composer.removePass(this.composer.passes[0]);
        }
        this.composer.dispose();
        
        // Dispose point cloud
        this.pointCloud.geometry.dispose();
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

    private getRelativeCoordinates(clientX: number, clientY: number) {
        const rect = this.canvas.getBoundingClientRect();
        return {
            x: clientX - rect.left,
            y: clientY - rect.top,
        };
    }

    private markInteraction(timestampMs: number = performance.now()) {
        this.lastInteractionTimestampMs = timestampMs;
        this.isIdle = false;
    }
}
