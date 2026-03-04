export function createWhiteNoise(audioContext: AudioContext) {
    const node = audioContext.createBufferSource();
    const buffer = audioContext.createBuffer(1, audioContext.sampleRate * 5, audioContext.sampleRate);
    const data = buffer.getChannelData(0);
    for (let i = 0; i < buffer.length; i++) {
        data[i] = Math.random() * 2 - 1;
    }
    node.buffer = buffer;
    node.loop = true;
    node.start(0);
    return node;
}
