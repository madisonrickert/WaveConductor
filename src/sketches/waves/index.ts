import * as THREE from "three";
import { lerp, map } from "@/common/math";
import { Sketch } from "@/common/sketch";
import { createAudioGroup, WavesSketchAudioGroup } from "./audio";
import { LeapHandController } from "@/common/leap/LeapHandController";
import { SettingDef } from "@/common/sketchSettings";
import { loadSettings } from "@/common/sketchSettingsStore";

const LINE_SEGMENT_LENGTH = (window.screen.width > 1024) ? 11 : 22;

/**
 * Procedural heightmap that drives the visual distortion of the line grid.
 * Combines a central bulb (logistic), radial waves (cosine), and a mouse-following ripple.
 * The mix between bulb and waves oscillates over time via {@link cachedWaviness}.
 */
export class HeightMap {
    /** Logical dimensions of the heightmap in world units, updated on resize. */
    width = 1200;
    height = 1200;
    /** Animation time counter, incremented each frame (by 1 normally, by 4 when mouse/touch is held). */
    frame = 0;

    /**
     * How wavy the heightmap is, from [0..1]. 0 means not wavy at all (only bulbous); 1.0 means only wavy.
     * Cached per-frame via {@link cacheFrame} to avoid recomputing per-vertex.
     */
    cachedWaviness = 0;

    /** Smoothed ripple center in world-space coordinates. Driven by hand/mouse position. */
    rippleCenterX = 0;
    rippleCenterY = 0;
    /** Target ripple center — the smoothed values lerp toward these each frame. */
    rippleTargetX = 0;
    rippleTargetY = 0;
    /**
     * Smoothed ripple amplitude. Default 800 (one hand or mouse).
     * With two hands, scales inversely with distance — closer = stronger/focused.
     */
    rippleAmplitude = 400;
    rippleTargetAmplitude = 400;

    /** Must be called once per frame before any evaluate/gradient calls. */
    cacheFrame() {
        this.cachedWaviness = (1 + Math.sin(this.frame / 100)) / 2;
        // Smooth ripple center and amplitude toward targets
        const smoothing = 0.15;
        this.rippleCenterX = lerp(this.rippleCenterX, this.rippleTargetX, smoothing);
        this.rippleCenterY = lerp(this.rippleCenterY, this.rippleTargetY, smoothing);
        this.rippleAmplitude = lerp(this.rippleAmplitude, this.rippleTargetAmplitude, smoothing);
    }

    /** Returns the height value at world-space position (x, y). */
    evaluate(x: number, y: number) {
        const length2 = x * x + y * y;
        // z1 creates the bulb shape at the center (using a logistic function)
        const z1 = 23000 / (1 + Math.exp(-length2 / 10000));
        // z2 creates the radial wave shapes from the center
        const z2 = 600 * Math.cos(length2 / 25000 + this.frame / 25);
        // z3 is a radial ripple centered on the ripple origin (hand/mouse position)
        const dx = x - this.rippleCenterX;
        const dy = y - this.rippleCenterY;
        const z3 = this.rippleAmplitude * Math.cos(Math.sqrt(dx * dx + dy * dy) / 20 - this.frame / 25);

        return lerp(z1, z2, this.cachedWaviness) + z3;
    }

    /** Returns the numerical gradient [dz/dx, dz/dy] at (x, y) via finite differences. */
    gradient(x: number, y: number) {
        const fnxy = this.evaluate(x, y);
        const epsilon = 1e-4;
        const ddx = (this.evaluate(x + epsilon, y) - fnxy) / epsilon;
        const ddy = (this.evaluate(x, y + epsilon) - fnxy) / epsilon;

        return [ddx, ddy];
    }
}

/**
 * Creates (or updates) a line geometry between two endpoints, displacing each vertex
 * along the heightmap gradient to produce a warped/distorted line effect.
 *
 * @param ox - Origin x
 * @param oy - Origin y
 * @param nx - End x
 * @param ny - End y
 * @param geometryIn - If provided, reuses this geometry buffer instead of allocating a new one.
 */
function permutedLine(heightMap: HeightMap, ox: number, oy: number, nx: number, ny: number, geometryIn?: THREE.BufferGeometry) {
    const ddx = nx - ox;
    const ddy = ny - oy;
    const distance = Math.sqrt(ddx * ddx + ddy * ddy);
    // about 11 units per line segment
    const steps = distance / LINE_SEGMENT_LENGTH;
    let geometry: THREE.BufferGeometry;
    if (geometryIn == null) {
        geometry = new THREE.BufferGeometry();
        const vertices = new Float32Array((steps + 1) * 3); // 3 components per vertex (x, y, z)
        geometry.setAttribute('position', new THREE.BufferAttribute(vertices, 3));
    } else {
        geometry = geometryIn;
    }

    const position = geometry.getAttribute('position') as THREE.BufferAttribute;
    for (let t = 0; t <= steps; t++) {
        const percentage = t / steps;
        const x = ox + ddx * percentage;
        const y = oy + ddy * percentage;
        const grad = heightMap.gradient(x, y);
        position.setXYZ(t, x + grad[0], y + grad[1], 0);
    }
    position.needsUpdate = true;
    return geometry;
}

/** A THREE.Line augmented with its grid position and inline extent, used for efficient per-frame updates. */
interface PositionedLine extends THREE.Line {
    /** Base grid position (before gridOffset is applied). */
    x: number;
    y: number;
    /** Half-extent of the line along its inline direction. */
    inlineOffsetX: number;
    inlineOffsetY: number;
}

/**
 * A set of parallel lines that tile the viewport along one direction.
 *
 * "Inline" is the direction each line draws along (defined by offsetX/offsetY in the constructor).
 * "Traversal" is the perpendicular direction in which lines are repeated at {@link gridSize} spacing.
 * Each frame, the entire strip shifts by (dx, dy) and wraps modulo gridSize, creating a scrolling effect.
 */
class LineStrip {
    /** Angle of the inline (line-drawing) direction in radians. */
    public inlineAngle: number;
    /** Per-frame velocity of the grid offset, set by mouse/touch/Leap position. */
    public dx: number;
    public dy: number;
    /** Current scroll offset of the line grid, wraps modulo gridSize. */
    public gridOffsetX: number;
    public gridOffsetY: number;
    /** Container for all THREE.Line children; added to the sketch scene. */
    public object: THREE.Object3D;

    constructor(private heightMap: HeightMap, public width: number, public height: number, offsetX: number, offsetY: number, public gridSize: number, private material: THREE.LineBasicMaterial) {
        this.inlineAngle = Math.atan(offsetY / offsetX);
        this.dx = 1;
        this.dy = 1;

        // the specific offset of the entire line for this frame
        this.gridOffsetX = 0;
        this.gridOffsetY = 0;

        this.object = new THREE.Object3D();

        this.resize(width, height);
    }

    public update() {
        this.gridOffsetX = ((this.gridOffsetX + this.dx) % this.gridSize + this.gridSize) % this.gridSize;
        this.gridOffsetY = ((this.gridOffsetY + this.dy) % this.gridSize + this.gridSize) % this.gridSize;
        (this.object.children as PositionedLine[]).forEach((lineMesh) => {
            const { x, y, inlineOffsetX, inlineOffsetY } = lineMesh;
            permutedLine(
                this.heightMap,
                x + this.gridOffsetX - inlineOffsetX,
                y + this.gridOffsetY - inlineOffsetY,
                x + this.gridOffsetX + inlineOffsetX,
                y + this.gridOffsetY + inlineOffsetY,
                lineMesh.geometry as THREE.BufferGeometry,
            );
        });
    }

    public resize(width: number, height: number) {
        this.width = width;
        this.height = height;

        // delete old lines
        this.object.remove(...this.object.children);

        const diagLength = Math.sqrt(this.width * this.width + this.height * this.height) + 2 * this.gridSize;
        // create and add a Line mesh to the lines array
        const createAndAddLine = (x: number, y: number) => {
            const inlineOffsetX = Math.cos(this.inlineAngle) * diagLength / 2;
            const inlineOffsetY = Math.sin(this.inlineAngle) * diagLength / 2;
            const geometry = permutedLine(
                this.heightMap,
                x - inlineOffsetX,
                y - inlineOffsetY,
                x + inlineOffsetX,
                y + inlineOffsetY,
            );
            const line = new THREE.Line(geometry, this.material);
            const lineMesh = Object.assign(line, {
                x,
                y,
                inlineOffsetX,
                inlineOffsetY,
                frustumCulled: false,
            }) as PositionedLine;
            this.object.add(lineMesh);
        };

        createAndAddLine(0, 0);

        const traversalAngle = this.inlineAngle + Math.PI / 2;
        for (let d = this.gridSize; d < diagLength / 2; d += this.gridSize) {
            createAndAddLine(+Math.cos(traversalAngle) * d,
                +Math.sin(traversalAngle) * d);
            createAndAddLine(-Math.cos(traversalAngle) * d,
                -Math.sin(traversalAngle) * d);
        }
    }
}

/**
 * Waves sketch — an interactive, generative line-art animation.
 *
 * Two orthogonal {@link LineStrip}s of parallel lines are distorted by a procedural {@link HeightMap}.
 * Mouse/touch/Leap input controls the scroll direction and speed of the line grid.
 * Holding down (mouse/touch) or squeezing with Leap scales animation speed (1–5×) and line opacity.
 * The color palette cycles between dark red and off-white over a 1000-frame period.
 */
export default class Waves extends Sketch {
    static id = "waves";
    static settings = {
        lineColor: { default: "#e9e9e9", category: "dev", label: "Line color", requiresRestart: true, type: "color" } satisfies SettingDef<string>,
        backgroundColor: { default: "#578fa0", category: "dev", label: "Background color", requiresRestart: true, type: "color" } satisfies SettingDef<string>,
    };

    private heightMap = new HeightMap();
    private lineStrips: LineStrip[] = [];
    /**
     * Continuous squeeze intensity in [0..1]. Controls animation speed (1–5×) and line opacity (0.03–0.23).
     * Set to 1 by mouse/touch hold, or mapped from Leap grab strength.
     */
    private speedFactor = 0;
    private lineMaterial = new THREE.LineBasicMaterial({ transparent: true, opacity: 0.03 });

    private leapHands!: LeapHandController;
    /** Hand wireframe color, derived from the background color in init(). */
    private _handColor = new THREE.Color();

    // Subtle per-frame fade toward background color to clear hand mesh smears
    private _fadeScene = new THREE.Scene();
    private _fadeCamera = new THREE.Camera();
    private _fadeMaterial = new THREE.MeshBasicMaterial({
        color: 0xffffff,
        transparent: true,
        opacity: 0.01,
        depthTest: false,
        depthWrite: false,
    });

    public events = {
        mousemove: (event: MouseEvent) => {
            this.setVelocityFromMouseEvent(event);
            this.markInteraction();
        },

        mousedown: (event: MouseEvent) => {
            if (event.button === 0) {
                this.speedFactor = 1;
                this.setVelocityFromMouseEvent(event);
                this.markInteraction();
            }
        },

        mouseup: (event: MouseEvent) => {
            if (event.button === 0) {
                this.speedFactor = 0;
                this.setVelocityFromMouseEvent(event);
            }
        },

        touchstart: (event: TouchEvent) => {
            // prevent emulated mouse events from occuring
            event.preventDefault();

            this.speedFactor = 1;
            this.setVelocityFromTouchEvent(event);
            this.markInteraction();
        },

        touchmove: (event: TouchEvent) => {
            this.setVelocityFromTouchEvent(event);
            this.markInteraction();
        },

        touchend: (_event: TouchEvent) => {
            this.speedFactor = 0;
        },
    };

    public audioGroup!: WavesSketchAudioGroup;
    public camera = new THREE.OrthographicCamera(0, 1, 0, 1, 1, 1000);
    public scene = new THREE.Scene();

    public init() {
        const { lineColor, backgroundColor } = loadSettings("waves", Waves.settings);
        this.lineMaterial.color.set(lineColor);
        this._fadeMaterial.color.set(backgroundColor);

        // Derive hand mesh color from background: lighten if dark, darken if bright.
        const bgHsl = { h: 0, s: 0, l: 0 };
        this._fadeMaterial.color.getHSL(bgHsl);
        const handL = bgHsl.l > 0.85 ? bgHsl.l - 0.2 : bgHsl.l + 0.3;
        this._handColor.setHSL(bgHsl.h, bgHsl.s * 0.5, Math.min(1, Math.max(0, handL)));

        this.audioGroup = createAudioGroup(this.audioContext, {
            heightMap: this.heightMap,
            getGrabStrength: () => this.speedFactor,
        });
        this.renderer.autoClearColor = false;

        this._fadeScene.add(new THREE.Mesh(new THREE.PlaneGeometry(2, 2), this._fadeMaterial));

        this.camera.position.z = 500;

        // cheap mobile detection
        const gridSize = (window.screen.width > 1024) ? 50 : 100;
        this.lineStrips.push(new LineStrip(this.heightMap, this.heightMap.width, this.heightMap.height, 1, -1, gridSize, this.lineMaterial));
        this.lineStrips.push(new LineStrip(this.heightMap, this.heightMap.width, this.heightMap.height, 0, 1, gridSize, this.lineMaterial));

        this.lineStrips.forEach((lineStrip) => {
            this.scene.add(lineStrip.object);
        });

        this.resize(this.renderer.domElement.width, this.renderer.domElement.height);

        // Leap Motion setup
        this.leapHands = new LeapHandController({
            canvas: this.canvas,
            renderer: this.renderer,
            getConnectionCallback: () => this.updateLeapConnectionCallback,
            getProtocolVersionCallback: () => this.updateLeapProtocolVersionCallback,
            renderMode: { type: "overlay" },
            handMaterial: new THREE.MeshBasicMaterial({
                color: this._handColor,
                wireframeLinewidth: 5,
                wireframe: true,
            }),
            onFrame: (hands) => {
                if (hands.length === 0) return;

                // Use the strongest grab across all hands as the speed factor
                const maxGrab = Math.max(...hands.map(({ hand }) => hand.grabStrength));
                this.speedFactor = maxGrab;

                if (hands.length === 1) {
                    // One hand: ripple follows hand, default amplitude
                    const pos = hands[0].canvasPosition;
                    this.setVelocityFromCanvasCoordinates(pos.x, pos.y);
                    this.heightMap.rippleTargetAmplitude = 400;
                } else {
                    // Two hands: ripple at midpoint, amplitude scales with proximity
                    const p0 = hands[0].canvasPosition;
                    const p1 = hands[1].canvasPosition;
                    const midX = (p0.x + p1.x) / 2;
                    const midY = (p0.y + p1.y) / 2;
                    this.setVelocityFromCanvasCoordinates(midX, midY);

                    // Distance in canvas pixels, normalized to canvas diagonal
                    const dx = p0.x - p1.x;
                    const dy = p0.y - p1.y;
                    const dist = Math.sqrt(dx * dx + dy * dy);
                    const diag = Math.sqrt(this.canvas.width * this.canvas.width + this.canvas.height * this.canvas.height);
                    const normalizedDist = dist / diag; // 0 = overlapping, ~1 = opposite corners

                    // Close together (0) → focused/strong (1000), far apart (1) → diffuse/subtle (150)
                    this.heightMap.rippleTargetAmplitude = lerp(1000, 150, normalizedDist);
                }
            },
        });
    }

    public animate() {
        const currentTimeMs = performance.now();

        // Check for Leap Motion interaction
        if (this.leapHands.activeHandCount > 0) {
            this.markInteraction(currentTimeMs);
        }

        if (!this.isIdle) {
            // Interpolate animation speed (1–5) and line opacity (0.03–0.23) based on squeeze intensity
            const targetOpacity = lerp(0.03, 0.23, this.speedFactor);
            const opacityChangeFactor = 0.1;
            this.lineMaterial.opacity = lerp(this.lineMaterial.opacity, targetOpacity, opacityChangeFactor);
            this.heightMap.frame += lerp(1, 5, this.speedFactor);

            this.heightMap.cacheFrame();
            this.audioGroup.updateParameters();

            const scale = map(Math.sin(this.heightMap.frame / 550), -1, 1, 1, 0.8);
            this.camera.scale.set(scale, scale, 1);
            this.lineStrips.forEach((lineStrip) => {
                lineStrip.update();
            });
            // Fade buffer toward background to gradually clear hand mesh smears
            this.renderer.autoClear = false;
            this.renderer.render(this._fadeScene, this._fadeCamera);
            this.renderer.autoClear = true;
            this.renderer.render(this.scene, this.camera);

            // Render hand meshes on top
            if (this.leapHands.activeHandCount > 0) {
                this.renderer.autoClearColor = true;
                this.leapHands.renderOverlay();
                this.renderer.autoClearColor = false;
            }
        }

        this.updateIdleState(currentTimeMs);
    }

    public resize(width: number, height: number) {
        if (width > height) {
            this.heightMap.height = 1200;
            this.heightMap.width = 1200 * width / height;
        } else {
            this.heightMap.width = 1200;
            this.heightMap.height = 1200 * height / width;
        }
        const camera = this.camera;
        camera.left = -this.heightMap.width / 2;
        camera.top = -this.heightMap.height / 2;
        camera.bottom = this.heightMap.height / 2;
        camera.right = this.heightMap.width / 2;
        camera.updateProjectionMatrix();

        this.renderer.setClearColor(this._fadeMaterial.color, 1);
        this.renderer.clear();

        // draw black again
        this.heightMap.frame = 0;

        this.lineStrips.forEach((lineStrip) => {
            lineStrip.resize(this.heightMap.width, this.heightMap.height);
        });

        this.leapHands?.resize(width, height);
    }

    setVelocityFromMouseEvent(event: MouseEvent) {
        const { x, y } = this.getRelativeCoordinates(event.clientX, event.clientY);
        this.setVelocityFromCanvasCoordinates(x, y);
    }

    setVelocityFromTouchEvent(event: TouchEvent) {
        const touch = event.touches[0];
        if (!touch) {
            return;
        }
        const { x, y } = this.getRelativeCoordinates(touch.clientX, touch.clientY);
        this.setVelocityFromCanvasCoordinates(x, y);
    }

    setVelocityFromCanvasCoordinates(canvasX: number, canvasY: number) {
        const dx = map(canvasX, 0, this.canvas.width, -1, 1) * 2.20;
        const dy = map(canvasY, 0, this.canvas.height, -1, 1) * 2.20;
        this.lineStrips.forEach((lineStrip) => {
            lineStrip.dx = dx;
            lineStrip.dy = dy;
        });
        this.setRippleTargetFromCanvasCoordinates(canvasX, canvasY);
    }

    /** Maps canvas pixel coordinates to heightmap world coordinates and sets the ripple target. */
    private setRippleTargetFromCanvasCoordinates(canvasX: number, canvasY: number) {
        this.heightMap.rippleTargetX = map(canvasX, 0, this.canvas.width, -this.heightMap.width / 2, this.heightMap.width / 2);
        this.heightMap.rippleTargetY = map(canvasY, 0, this.canvas.height, -this.heightMap.height / 2, this.heightMap.height / 2);
    }

    public destroy() {
        // Clean up audio resources (also removes the <audio> DOM element)
        this.audioGroup.dispose();

        // Clean up Leap Motion controller
        this.leapHands.dispose();

        // Clean up Three.js resources
        this.lineStrips.forEach((lineStrip) => {
            this.scene.remove(lineStrip.object);
            // Dispose geometry for each line in the strip
            lineStrip.object.children.forEach((child) => {
                if (child instanceof THREE.Line) {
                    child.geometry.dispose();
                }
            });
        });
        this.lineMaterial.dispose();
        this.lineStrips.length = 0;
        this._fadeMaterial.dispose();
    }
}
