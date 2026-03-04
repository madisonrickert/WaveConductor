// AudioWorklet processor — runs on the audio render thread, NOT the main thread.
// Cannot import from other project modules.

class WavesBiquadProcessor extends AudioWorkletProcessor {
  private y1 = 0;

  static get parameterDescriptors() {
    return [
      { name: 'a0', defaultValue: 0, minValue: 0, maxValue: 1, automationRate: 'k-rate' as AutomationRate },
      { name: 'b1', defaultValue: 0, minValue: -1, maxValue: 0, automationRate: 'k-rate' as AutomationRate },
    ];
  }

  process(inputs: Float32Array[][], outputs: Float32Array[][], parameters: Record<string, Float32Array>): boolean {
    const input = inputs[0]?.[0];
    const output = outputs[0]?.[0];
    if (!input || !output) return true;

    const a0 = parameters.a0[0];
    const b1 = parameters.b1[0];

    for (let i = 0; i < output.length; i++) {
      const y = a0 * input[i] - b1 * this.y1;
      output[i] = y;
      this.y1 = y;
    }

    return true;
  }
}

registerProcessor('waves-biquad-processor', WavesBiquadProcessor);
