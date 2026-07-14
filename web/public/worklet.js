class MicTap extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0]
    if (ch && ch.length) {
      this.port.postMessage(new Float32Array(ch))
    }
    return true
  }
}
registerProcessor('mic-tap', MicTap)
