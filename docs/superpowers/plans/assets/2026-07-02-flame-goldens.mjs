// Golden-value generator for the Flame v4->v5 port.
// Mirrors .worktrees/v4/src/sketches/flame/index.tsx (stringHash, randomBranch*)
// and src/math.ts (map) EXACTLY, with THREE.js scalar math inlined.
// All arithmetic is IEEE-754 f64, which Rust reproduces bit-for-bit.
const GEN_DIVISOR = 2147483648 - 1; // 2^31 - 1
const AFFINE_KEYS = ["TowardsOriginNegativeBias", "TowardsOrigin2", "Swap", "SwapSub", "Negate", "NegateSwap", "Up1"];
const VAR_KEYS = ["Linear", "Sin", "Spherical", "Polar", "Swirl", "Normalize", "Shrink"];
function stringHash(s) {
    let hash = 0, char;
    if (s.length === 0) { return hash; }
    for (let i = 0, l = s.length; i < l; i++) {
        char = s.charCodeAt(i);
        hash = hash * 31 + char;
        hash |= 0; // ToInt32 wrap
    }
    hash *= hash * 31;
    return hash;
}
function map(x, xStart, xStop, yStart, yStop) {
    return yStart + (yStop - yStart) * ((x - xStart) / (xStop - xStart));
}
function randomBranch(idx, substring, numBranches, numWraps) {
    let gen = stringHash(substring);
    function next() { return (gen = (gen * 4194303 + 127) % GEN_DIVISOR); }
    for (let i = 0; i < 5 + idx * numWraps; i++) { next(); }
    const newVariationIdx = () => { next(); return gen % VAR_KEYS.length; };
    const random = () => { next(); return gen / GEN_DIVISOR; };
    const affineIdx = gen % AFFINE_KEYS.length; // gen as left by the skip loop
    const varA = newVariationIdx();
    let mode = 0, varB = -1;
    if (random() < numWraps * 0.25) {
        mode = 1; varB = newVariationIdx();
    } else if (numWraps > 2 && random() < 0.2) {
        mode = 2; varB = newVariationIdx();
    }
    const colorValues = [random() * 0.1 - 0.05, random() * 0.1 - 0.05, random() * 0.1 - 0.05];
    colorValues[idx % 3] += 0.2;
    const scale = numBranches / 3.5;
    return { affine_idx: affineIdx, var_a_idx: varA, mode, var_b_idx: varB,
             color: colorValues.map((c) => c * scale) };
}
function goldensFor(name) {
    const numWraps = Math.floor(name.length / 5);
    const numBranches = Math.ceil(1 + (name.length % 5) + numWraps);
    const branches = [];
    for (let i = 0; i < numBranches; i++) {
        const stringStart = map(i, 0, numBranches, 0, name.length);
        const stringEnd = map(i + 1, 0, numBranches, 0, name.length);
        const substring = name.substring(stringStart, stringEnd);
        branches.push({ substring, ...randomBranch(i, substring, numBranches, numWraps) });
    }
    const hash = stringHash(name);
    const hashNorm = (hash % 1024) / 1024;
    const hash2 = hash * hash + hash * 31 + 9;
    const hash3 = hash2 * hash2 + hash2 * 31 + 9;
    return {
        name, num_wraps: numWraps, num_branches: numBranches,
        depth: Math.floor(Math.log(100000) / Math.log(numBranches)),
        hash, hash2, hash3,
        c_y: map(hashNorm, 0, 1, -2.5, 2.5),
        filter_freq: map((hash2 % 2e12) / 2e12, 0, 1, 120, 400),
        filter_q: map((hash3 % 2e12) / 2e12, 0, 1, 5, 8),
        noise_gain_scale: map(((hash2 * hash3) % 100) / 100, 0, 1, 0.5, 1),
        is_major: hash2 % 2 === 0,
        has_noise: (hash3 % 100) >= 50,
        branches,
    };
}
function prngSequence(seedString, n) {
    let gen = stringHash(seedString);
    const out = [];
    for (let i = 0; i < n; i++) { gen = (gen * 4194303 + 127) % GEN_DIVISOR; out.push(gen); }
    return { seed_string: seedString, seed_hash: stringHash(seedString), sequence: out };
}
const result = {
    prng: prngSequence("who ", 8),
    names: ["who are you?", "madison", "a", "xy", "abcdefghijklmnopqrs", "Xiaohan"].map(goldensFor),
};
console.log(JSON.stringify(result, null, 1));
