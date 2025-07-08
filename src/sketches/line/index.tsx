import * as THREE from "three";
import { RenderPass, EffectComposer } from "three-stdlib";
import queryString from "query-string";
import { GravityShaderPass } from "@/common/shaders/gravity";
import { computeStats, createParticle, createParticlePoints, IParticle, ParticleSystem } from "@/common/particleSystem";
import { Attractor } from "@/common/particleSystem/attractor";
import { triangleWaveApprox } from "@/common/math";
import { ISketch } from "@/sketch";
import { createAudioGroup } from "./audio";
import { starMaterial } from "@/common/materials/starMaterial";
import { ScreenSaver } from "@/common/screenSaver/screenSaver";
import { LeapAttractorController } from "./LeapAttractorController";
import { AudioGroup } from "./types";

const PARTICLE_SYSTEM_PARAMS = {
    GRAVITY_CONSTANT: 280,
    INERTIAL_DRAG_CONSTANT: 0.53913643334,
    PULLING_DRAG_CONSTANT: 0.93075095702,
    timeStep: 0.016 * 2,
    STATIONARY_CONSTANT: 0.0,
    constrainToBox: true,
};

export default class LineSketch extends ISketch {
    public events = {
        touchstart: (event: JQuery.Event) => {
            // Prevent emulated mouse events from occuring
            event.preventDefault();
            const touch = ((event as JQuery.TouchStartEvent).originalEvent as TouchEvent).touches[0];
            const touchX = touch.pageX;
            let touchY = touch.pageY;
            // Offset the touchY by its radius so the attractor is above the thumb
            touchY -= 100;

            this.setGravityFocalPoint(touchX, touchY);
            this.enableMouseAttractor(touchX, touchY);
        },

        touchmove: (event: JQuery.Event) => {
            const touch = ((event as JQuery.TouchMoveEvent).originalEvent as TouchEvent).touches[0];
            const touchX = touch.pageX;
            let touchY = touch.pageY;
            touchY -= 100;

            this.setGravityFocalPoint(touchX, touchY);
            this.moveMouseAttractor(touchX, touchY);
        },

        touchend: (_event: JQuery.Event) => {
            this.disableMouseAttractor();
        },

        mousedown: (event: JQuery.Event) => {
            if (event.which === 1) {
                const mouseEvent = event as JQuery.Event & { originalEvent: MouseEvent };
                const x = event.offsetX == null ? mouseEvent.originalEvent.layerX : event.offsetX;
                const y = event.offsetY == null ? mouseEvent.originalEvent.layerY : event.offsetY;
                this.setGravityFocalPoint(x, y);
                this.enableMouseAttractor(x, y);
            }
        },

        mousemove: (event: JQuery.Event) => {
            const mouseEvent = event as JQuery.Event & { originalEvent: MouseEvent };
            const x = event.offsetX == null ? mouseEvent.originalEvent.layerX : event.offsetX;
            const y = event.offsetY == null ? mouseEvent.originalEvent.layerY : event.offsetY;
            this.setGravityFocalPoint(x, y);
            this.moveMouseAttractor(x, y);
        },

        mouseup: (event: JQuery.Event) => {
            if (event.which === 1) {
                this.disableMouseAttractor();
            }
        },
    };
    public elements = [
        <ScreenSaver
            ref={(screenSaver: ScreenSaver) => { this.screenSaver = screenSaver; }}
            shouldShow={false} // Placeholder, will be updated dynamically
        />
    ];
    public screenSaver: ScreenSaver | null = null;

    // TODO move into core sketch
    public globalFrame = 0;
    public lastRenderedFrame = -Infinity;

    public audioGroup!: AudioGroup;
    public particles: IParticle[] = [];

    // Three.js & Rendering
    public mouseAttractor: Attractor = new Attractor();
    public leapAttractors: Attractor[] = [];
    public camera = new THREE.OrthographicCamera(0, 0, 0, 0, 1, 1000);
    public gravityShaderPass = new GravityShaderPass();
    public gravityFocalX = 0;
    public gravityFocalY = 0;
    public scene = new THREE.Scene();
    public points!: THREE.Points;
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
        // Set up audio
        this.audioGroup = createAudioGroup(this.audioContext);

        // Set up camera and scene
        this.resize(this.canvas.width, this.canvas.height);
        this.camera.position.z = 500;

        // Add mouse attractor mesh to scene
        this.scene.add(this.mouseAttractor.ringMeshesGroup);

        // Determine number of particles (query param or device type)
        const NUM_PARTICLES = Number(queryString.parse(location.search).p) ||
            // cheap mobile detection
            (screen.width > 1024 ? 20000 : 5000);
        
        // Evenly space particles across the middle of the screen in a line
        for (let i = 0; i < NUM_PARTICLES; i++) {
            this.particles.push(createParticle(
                i / NUM_PARTICLES * this.canvas.width,
                this.canvas.height / 2 + ((i % 5) - 2) * 2, // Very subtle sawtooth wave
            ));
        }

        // Set up particle system and points
        this.ps = new ParticleSystem(
            this.canvas,
            this.particles,
            PARTICLE_SYSTEM_PARAMS,
        );
        this.points = createParticlePoints(this.particles, starMaterial);
        this.scene.add(this.points);

        // Set up postprocessing composer and passes
        this.composer = new EffectComposer(this.renderer);
        this.composer.addPass(new RenderPass(this.scene, this.camera));
        this.gravityShaderPass.uniforms.iResolution.value = new THREE.Vector2(this.canvas.width, this.canvas.height);
        const gamma = queryString.parse(location.search).gamma;
        if (gamma) {
            this.gravityShaderPass.uniforms.gamma.value = gamma;
        }
        this.gravityShaderPass.renderToScreen = true;
        this.composer.addPass(this.gravityShaderPass);

        // Set up Leap Motion controller
        this.leapAttractorController = new LeapAttractorController(this);
        this.leapAttractorController.attachToLeap();
    }

    public animate(_millisElapsed: number) {
        // Animate all attractors
        this.mouseAttractor.animate(_millisElapsed);
        for (const attractor of this.leapAttractors) {
            attractor.animate(_millisElapsed);
        }

        // Use the focal point set by setGravityFocalPoint
        this.gravityShaderPass.uniforms.iMouse.value.set(
            this.gravityFocalX,
            this.renderer.domElement.height - this.gravityFocalY
        );

        // Step particles with all active attractors
        const activeAttractors = [
            ...(this.mouseAttractor.power !== 0 ? [this.mouseAttractor] : []),
            ...this.leapAttractors.filter((attractor) => attractor.power !== 0)
        ];
        this.ps.stepParticles(activeAttractors);

        // Update particle positions in geometry
        // @todo Move to ParticleSystem
        const positionAttr = this.points.geometry.getAttribute('position');
        for (let i = 0; i < this.particles.length; i++) {
            const particle = this.particles[i];
            positionAttr.setXY(i, particle.x, particle.y);
        }
        positionAttr.needsUpdate = true;

        // --- Audio Feedback ---
        const {
            groupedUpness,
            normalizedVarianceLength,
            flatRatio,
            normalizedEntropy
        } = computeStats(this.ps);

        this.audioGroup.sourceLfo.frequency.setTargetAtTime(flatRatio, 0, 0.016);
        if (normalizedEntropy !== 0) {
            this.audioGroup.setFrequency(222 / normalizedEntropy);
        }
        const noiseFreq = 2000 * normalizedVarianceLength;
        this.audioGroup.setNoiseFrequency(noiseFreq);
        this.audioGroup.setVolume(Math.max(groupedUpness - 0.05, 0) * 5.);
        const backgroundVolume = 1.00;
        this.audioGroup.setBackgroundVolume(backgroundVolume);

        // --- Shader Uniforms ---
        this.gravityShaderPass.uniforms.iGlobalTime.value = performance.now() / 1000;
        this.gravityShaderPass.uniforms.G.value = triangleWaveApprox(performance.now() / 5000) * (groupedUpness + 0.50) * 15000;
        this.gravityShaderPass.uniforms.iMouseFactor.value = (1 / 15) / (groupedUpness + 1);

        // --- Render ---
        this.composer.render();
        this.globalFrame++;

        // --- Screen Saver Logic ---
        if (this.screenSaver != null) {
            const isLeapMotionControllerValid = this.leapAttractorController.lastFrameIsValid();
            const numSecondsToShowScreenSaver = 10;
            const shouldShow =
                !(this.globalFrame - this.lastRenderedFrame < 60 * numSecondsToShowScreenSaver) &&
                isLeapMotionControllerValid;

            this.screenSaver.setState({ shouldShow }); // Dynamically update shouldShow
        }
    }

    public resize(width: number, height: number) {
        this.camera.right = width;
        this.camera.bottom = height;
        this.camera.updateProjectionMatrix();
        this.gravityShaderPass.uniforms.iResolution.value = new THREE.Vector2(width, height);
    }

    public setGravityFocalPoint(x: number, y: number) {
        this.gravityFocalX = x;
        this.gravityFocalY = y;
    }

    // --- Attractor Controls ---
    private enableMouseAttractor(x: number, y: number) {
        this.mouseAttractor.x = x;
        this.mouseAttractor.y = y;
        this.mouseAttractor.power = 20;
    }

    private moveMouseAttractor(x: number, y: number) {
        this.mouseAttractor.x = x;
        this.mouseAttractor.y = y;
    }

    private disableMouseAttractor() {
        this.mouseAttractor.power = 0;
    }
}
