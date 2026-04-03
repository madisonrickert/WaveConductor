import * as THREE from "three";
import { EffectComposer, RenderPass } from "three-stdlib";

import { ExplodeShaderPass } from "./shaders/explode";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem, ParticleSystemParameters } from "@/particles";
import { Attractor } from "@/particles";
import { loadSettings } from "@/settings/store";
import { SettingDef } from "@/settings/types";
import { BaseSketch } from "@/sketch/BaseSketch";
import { disposeComposer } from "@/sketch/disposeComposer";
import { createAudioGroup, DotSketchAudioGroup } from "./audio";
import { starMaterial } from "@/materials/starMaterial";

const params: ParticleSystemParameters = {
    timeStep: 0.016 * 3,
    GRAVITY_CONSTANT: 100,
    PULLING_DRAG_CONSTANT: 0.96075095702,
    INERTIAL_DRAG_CONSTANT: 0.23913643334,
    STATIONARY_CONSTANT: 0.01,
    FADE_DURATION: 3,
    constrainToBox: false,
};

const ATTRACTOR_POWER_DECAY_SPEED = 0.9;
const ATTRACTOR_POWER_DECAY_FLOOR = 2;

const LEAP_ATTRACTOR_POWER_ATTACK_SPEED = 0.005;
const LEAP_ATTRACTOR_POWER_DECAY_SPEED = 0.5;
const LEAP_ATTRACTOR_POWER_THRESHOLD = 0.05;

export default class DotsSketch extends BaseSketch {
    static id = "dots";
    static settings = {
        dotSpacing: { default: 20, category: "dev", label: "Dot spacing (px)", requiresRestart: true } satisfies SettingDef<number>,
        gamma: { default: 1.0, category: "dev", label: "Gamma", requiresRestart: true, step: 0.1 } satisfies SettingDef<number>,
    };
    private attractor = new Attractor();
    private leapAttractors: Attractor[] = [];
    private mouseX = 0;
    private mouseY = 0;
    public events = {
        touchstart: (event: TouchEvent) => {
            // prevent emulated mouse events from occuring
            event.preventDefault();
            const touch = event.touches[0];
            if (!touch) {
                return;
            }
            const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
            this.createAttractor(x, y);
            this.mouseX = x;
            this.mouseY = y;
            this.markInteraction();
        },

        touchmove: (event: TouchEvent) => {
            const touch = event.touches[0];
            if (!touch) {
                return;
            }
            const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
            this.moveAttractor(x, y);
            this.mouseX = x;
            this.mouseY = y;
            this.markInteraction();
        },

        touchend: (_event: TouchEvent) => {
            this.removeAttractor();
        },

        mousedown: (event: MouseEvent) => {
            if (event.button === 0) {
                const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
                this.mouseX = x;
                this.mouseY = y;
                this.createAttractor(this.mouseX, this.mouseY);
                this.markInteraction();
            }
        },

        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.mouseX = x;
            this.mouseY = y;
            this.moveAttractor(this.mouseX, this.mouseY);
            this.markInteraction();
        },

        mouseup: (event: MouseEvent) => {
            if (event.button === 0) {
                this.removeAttractor();
            }
        },
    };

    public shader = new ExplodeShaderPass();
    public audioGroup!: DotSketchAudioGroup;
    public camera!: THREE.OrthographicCamera;
    public composer!: EffectComposer;
    public pointCloud!: THREE.Points;
    public scene = new THREE.Scene();
    public ps!: ParticleSystem;

    public init() {
        this.audioGroup = createAudioGroup(this.audioContext);

        this.camera = new THREE.OrthographicCamera(0, this.canvas.width, 0, this.canvas.height, 1, 1000);
        this.camera.position.z = 500;

        const particles: IParticle[] = [];
        const EXTENT = 10;
        const settings = loadSettings("dots", DotsSketch.settings);
        const dotSpacing = settings.dotSpacing;
        for (let x = -EXTENT * dotSpacing; x < this.canvas.width + EXTENT * dotSpacing; x += dotSpacing) {
            for (let y = -EXTENT * dotSpacing; y < this.canvas.height + EXTENT * dotSpacing; y += dotSpacing) {
                particles.push(createParticle(x, y));
            }
        }
        this.ps = new ParticleSystem(this.canvas, particles, params);

        this.pointCloud = createParticlePoints(particles, starMaterial);
        this.scene.add(this.pointCloud);

        this.composer = new EffectComposer(this.renderer);
        this.composer.addPass(new RenderPass(this.scene, this.camera));
        this.shader.uniforms.iResolution.value = this.resolution;
        this.shader.uniforms.gamma.value = settings.gamma;
        this.shader.renderToScreen = true;
        this.composer.addPass(this.shader);

        // Leap Motion setup
        this.leapHands = this.createLeapController({
            renderMode: { type: "in-scene", scene: this.scene },
            onFrame: (hands) => {
                hands.forEach(({ hand, index, canvasPosition }) => {
                    const attractor = this.getLeapAttractor(index);
                    attractor.x = canvasPosition.x;
                    attractor.y = canvasPosition.y;
                    const position = hand.palmPosition;
                    if (hand.grabStrength < 0.1) {
                        attractor.power *= LEAP_ATTRACTOR_POWER_DECAY_SPEED;
                        if (attractor.power < LEAP_ATTRACTOR_POWER_THRESHOLD) {
                            attractor.power = 0;
                        }
                    } else {
                        const grabComponent = Math.pow(hand.grabStrength, 1.5);
                        const depthModulator = Math.pow(5, (-position[2] + 350) / 160);
                        const wantedPower = grabComponent * depthModulator;
                        attractor.power = attractor.power * (1 - LEAP_ATTRACTOR_POWER_ATTACK_SPEED) + wantedPower * LEAP_ATTRACTOR_POWER_ATTACK_SPEED;
                    }
                    if (index === 0) {
                        this.mouseX = canvasPosition.x;
                        this.mouseY = canvasPosition.y;
                    }
                });
                for (let i = hands.length; i < this.leapAttractors.length; i++) {
                    this.leapAttractors[i].power = 0;
                }
            },
        });
    }

    protected step(): void {
        const nonzeroAttractors: Attractor[] = [];
        if (this.attractor.power > 0) {
            nonzeroAttractors.push(this.attractor);
        }
        for (const leapAttractor of this.leapAttractors) {
            if (leapAttractor.power >= LEAP_ATTRACTOR_POWER_THRESHOLD) {
                nonzeroAttractors.push(leapAttractor);
            }
        }
        this.ps.stepParticles(nonzeroAttractors, this.pointCloud);

        const { flatRatio, normalizedVarianceLength, groupedUpness, averageVel } = computeStats(this.ps);
        this.audioGroup.lfo.frequency.cancelScheduledValues(this.audioContext.currentTime);
        this.audioGroup.lfo.frequency.setTargetAtTime(flatRatio, this.audioContext.currentTime, 0.016);
        this.audioGroup.setFrequency(120 / normalizedVarianceLength * averageVel / 100 );
        this.audioGroup.setVolume(Math.max(groupedUpness - 0.05, 0));

        this.shader.uniforms.iMouse.value = new THREE.Vector2(this.mouseX / this.canvas.width, (this.canvas.height - this.mouseY) / this.canvas.height);

        this.composer.render();

        if (this.attractor.power > 0) {
            this.attractor.power =
                ATTRACTOR_POWER_DECAY_FLOOR +
                (this.attractor.power - ATTRACTOR_POWER_DECAY_FLOOR) * ATTRACTOR_POWER_DECAY_SPEED;
        }
    }

    public resize(width: number, height: number) {
        const { camera, shader } = this;
        camera.right = width;
        camera.bottom = height;
        shader.uniforms.iResolution.value = new THREE.Vector2(width, height);

        camera.updateProjectionMatrix();
        this.composer?.setSize(width, height);
    }

    public destroy() {
        super.destroy();
        this.audioGroup.dispose();
        disposeComposer(this.composer);
        this.pointCloud.geometry.dispose();
        this.scene.remove(this.pointCloud);
    }

    private getLeapAttractor(index: number): Attractor {
        while (this.leapAttractors.length <= index) {
            this.leapAttractors.push(new Attractor());
        }
        return this.leapAttractors[index];
    }

    private createAttractor(x: number, y: number) {
        this.attractor.x = x;
        this.attractor.y = y;
        this.attractor.power = 1;
    }

    private moveAttractor(x: number, y: number) {
        this.attractor.x = x;
        this.attractor.y = y;
    }

    private removeAttractor() {
        this.attractor.power = 0;
    }

    private hasActiveAttractors(): boolean {
        if (this.attractor.power > ATTRACTOR_POWER_DECAY_FLOOR + 1e-2) {
            return true;
        }
        return this.leapAttractors.some((attractor) => attractor.power >= LEAP_ATTRACTOR_POWER_THRESHOLD);
    }

    protected isReadyToSleep(): boolean {
        return !this.hasActiveAttractors();
    }
}
