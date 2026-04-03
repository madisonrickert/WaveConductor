import { createParticle, IParticle } from "@/particles";

/**
 * Sample particle positions from an image used as a brightness heatmap.
 * Brighter pixels have a higher probability of spawning a particle.
 * Uses CDF-based weighted random sampling for accurate distribution.
 */
export function sampleParticlesFromHeatmap(
    img: HTMLImageElement,
    canvasWidth: number,
    canvasHeight: number,
    particleCount: number,
): IParticle[] {
    // Draw image scaled to canvas size on an offscreen canvas
    const offscreen = document.createElement("canvas");
    // Use a lower resolution for the sampling grid to keep memory reasonable
    const sampleWidth = Math.min(canvasWidth, 256);
    const sampleHeight = Math.min(canvasHeight, 256);
    offscreen.width = sampleWidth;
    offscreen.height = sampleHeight;
    const ctx = offscreen.getContext("2d")!;
    ctx.drawImage(img, 0, 0, sampleWidth, sampleHeight);
    const imageData = ctx.getImageData(0, 0, sampleWidth, sampleHeight);
    const data = imageData.data;

    // Build CDF from pixel brightness (luminance)
    const totalPixels = sampleWidth * sampleHeight;
    const cdf = new Float64Array(totalPixels);
    let cumulative = 0;
    for (let i = 0; i < totalPixels; i++) {
        const idx = i * 4;
        const luminance = 0.299 * data[idx] + 0.587 * data[idx + 1] + 0.114 * data[idx + 2];
        // Multiply by alpha so transparent pixels don't spawn particles
        const alpha = data[idx + 3] / 255;
        cumulative += luminance * alpha;
        cdf[i] = cumulative;
    }

    if (cumulative === 0) {
        // All-black or fully transparent image — fall back to center line
        const particles: IParticle[] = [];
        for (let i = 0; i < particleCount; i++) {
            particles.push(createParticle(
                i / particleCount * canvasWidth,
                canvasHeight / 2,
            ));
        }
        return particles;
    }

    // Sample particles using binary search on the CDF
    const particles: IParticle[] = [];
    const scaleX = canvasWidth / sampleWidth;
    const scaleY = canvasHeight / sampleHeight;

    for (let i = 0; i < particleCount; i++) {
        const target = Math.random() * cumulative;
        // Binary search for the pixel index
        let lo = 0, hi = totalPixels - 1;
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (cdf[mid] < target) lo = mid + 1;
            else hi = mid;
        }
        const px = lo % sampleWidth;
        const py = Math.floor(lo / sampleWidth);
        // Map back to canvas coordinates with sub-pixel jitter
        const x = (px + Math.random()) * scaleX;
        const y = (py + Math.random()) * scaleY;
        particles.push(createParticle(x, y));
    }
    return particles;
}
