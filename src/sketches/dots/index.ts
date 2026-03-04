import * as THREE from "three";
import { EffectComposer, RenderPass } from "three-stdlib";

import { ExplodeShaderPass } from "./shaders/explode";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem, ParticleSystemParameters } from "@/common/particleSystem";
import { Attractor } from "@/common/particleSystem";
import { loadSettings } from "@/common/sketchSettingsStore";
import { SettingDef } from "@/common/sketchSettings";
import { Sketch } from "@/common/sketch";
import { createAudioGroup, DotSketchAudioGroup } from "./audio";
import { starMaterial } from "@/common/materials/starMaterial";

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

export default class Dots extends Sketch {
    static id = "dots";
    static settings = {
        dotSpacing: { default: 20, category: "dev", label: "Dot spacing (px)", requiresRestart: true } satisfies SettingDef<number>,
        gamma: { default: 1.0, category: "dev", label: "Gamma", requiresRestart: true, step: 0.1 } satisfies SettingDef<number>,
    };
    private attractor = new Attractor();
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
            }
        },

        mousemove: (event: MouseEvent) => {
            const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
            this.mouseX = x;
            this.mouseY = y;
            this.moveAttractor(this.mouseX, this.mouseY);
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
        const settings = loadSettings("dots", Dots.settings);
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
    }

    public animate(_millisElapsed: number) {
        const nonzeroAttractors = this.attractor.power > 0 ? [this.attractor] : [];
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
        // Clean up audio resources
        this.audioGroup.dispose();

        // Clean up Three.js resources
        this.pointCloud.geometry.dispose();
        this.scene.remove(this.pointCloud);
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
}
