import queryString from "query-string";
import * as THREE from "three";
import { EffectComposer, RenderPass } from "three-stdlib";

import { ExplodeShaderPass } from "@/common/shaders/explode";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem, ParticleSystemParameters } from "@/common/particleSystem";
import { Attractor } from "@/common/particleSystem";
import { ISketch } from "@/sketch";
import { createAudioGroup, DotSketchAudioGroup } from "./audio";
import { starMaterial } from "@/common/materials/starMaterial";

const params: ParticleSystemParameters = {
    timeStep: 0.016 * 3,
    GRAVITY_CONSTANT: 100,
    PULLING_DRAG_CONSTANT: 0.96075095702,
    INERTIAL_DRAG_CONSTANT: 0.23913643334,
    STATIONARY_CONSTANT: 0.01,
    constrainToBox: false,
};

const ATTRACTOR_POWER_DECAY_SPEED = 0.9;
const ATTRACTOR_POWER_DECAY_FLOOR = 2;

const attractor = new Attractor();
let mouseX: number, mouseY: number;

function getRelativePoint(target: EventTarget | null, clientX: number, clientY: number) {
    if (target instanceof HTMLElement) {
        const rect = target.getBoundingClientRect();
        return {
            x: clientX - rect.left,
            y: clientY - rect.top,
        };
    }
    return { x: clientX, y: clientY };
}

function touchstart(event: TouchEvent) {
    // prevent emulated mouse events from occuring
    event.preventDefault();
    const touch = event.touches[0];
    if (!touch) {
        return;
    }
    const { x, y } = getRelativePoint(event.currentTarget, touch.clientX, touch.clientY);
    createAttractor(x, y);
    mouseX = x;
    mouseY = y;
}

function touchmove(event: TouchEvent) {
    const touch = event.touches[0];
    if (!touch) {
        return;
    }
    const { x, y } = getRelativePoint(event.currentTarget, touch.clientX, touch.clientY);
    moveAttractor(x, y);
    mouseX = x;
    mouseY = y;
}

function touchend(_event: TouchEvent) {
    removeAttractor();
}

function mousedown(event: MouseEvent) {
    if (event.button === 0) {
        const { x, y } = getRelativePoint(event.currentTarget, event.clientX, event.clientY);
        mouseX = x;
        mouseY = y;
        createAttractor(mouseX, mouseY);
    }
}

function mousemove(event: MouseEvent) {
    const { x, y } = getRelativePoint(event.currentTarget, event.clientX, event.clientY);
    mouseX = x;
    mouseY = y;
    moveAttractor(mouseX, mouseY);
}

function mouseup(event: MouseEvent) {
    if (event.button === 0) {
        removeAttractor();
    }
}

function createAttractor(x: number, y: number) {
    attractor.x = x;
    attractor.y = y;
    attractor.power = 1;
}

function moveAttractor(x: number, y: number) {
    if (attractor != null) {
        attractor.x = x;
        attractor.y = y;
    }
}

function removeAttractor() {
    attractor.power = 0;
}

export default class Dots extends ISketch {
    public events = {
        mousedown,
        mousemove,
        mouseup,
        touchstart,
        touchmove,
        touchend,
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
        const GRID_SIZE: number = Number(queryString.parse(location.search).gridSize) || 7;
        for (let x = -EXTENT * GRID_SIZE; x < this.canvas.width + EXTENT * GRID_SIZE; x += GRID_SIZE) {
            for (let y = -EXTENT * GRID_SIZE; y < this.canvas.height + EXTENT * GRID_SIZE; y += GRID_SIZE) {
                particles.push(createParticle(x, y));
            }
        }
        this.ps = new ParticleSystem(this.canvas, particles, params);

        this.pointCloud = createParticlePoints(particles, starMaterial);
        this.scene.add(this.pointCloud);

        this.composer = new EffectComposer(this.renderer);
        this.composer.addPass(new RenderPass(this.scene, this.camera));
        this.shader.uniforms.iResolution.value = this.resolution;
        this.shader.renderToScreen = true;
        this.composer.addPass(this.shader);
    }

    public animate(_millisElapsed: number) {
        const nonzeroAttractors = attractor.power > 0 ? [attractor] : [];
        this.ps.stepParticles(nonzeroAttractors, this.pointCloud);

        const { flatRatio, normalizedVarianceLength, groupedUpness, averageVel } = computeStats(this.ps);
        this.audioGroup.lfo.frequency.setTargetAtTime(flatRatio, this.audioContext.currentTime, 0.016);
        this.audioGroup.setFrequency(120 / normalizedVarianceLength * averageVel / 100 );
        this.audioGroup.setVolume(Math.max(groupedUpness - 0.05, 0));

        this.shader.uniforms.iMouse.value = new THREE.Vector2(mouseX / this.canvas.width, (this.canvas.height - mouseY) / this.canvas.height);

        this.composer.render();

        if (attractor.power > 0) {
            attractor.power =
                ATTRACTOR_POWER_DECAY_FLOOR +
                (attractor.power - ATTRACTOR_POWER_DECAY_FLOOR) * ATTRACTOR_POWER_DECAY_SPEED;
        }
    }

    public resize(width: number, height: number) {
        const { camera, shader } = this;
        camera.right = width;
        camera.bottom = height;
        shader.uniforms.iResolution.value = new THREE.Vector2(width, height);

        camera.updateProjectionMatrix();
    }

    public destroy() {
        // Clean up audio resources
        this.audioGroup.dispose();

        // Clean up Three.js resources
        this.pointCloud.geometry.dispose();
        this.scene.remove(this.pointCloud);
    }
}
