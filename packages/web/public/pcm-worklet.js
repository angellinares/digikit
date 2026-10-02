// Consumes interleaved stereo f32 PCM blocks (48 kHz; the AudioContext is
// created at that rate so the browser resamples to the device) posted from the
// emulator worker. Playback starts once `startSeconds` of audio are queued
// ("buffer N seconds, then play"); on underrun it outputs silence and waits
// for the same amount again, so an emulator that is slower than real time
// plays in bursts instead of stuttering.
class PcmProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.head = 0; // read offset (floats) into queue[0]
    this.queued = 0; // frames (L/R pairs) waiting
    this.playing = false;
    this.start = 0.5 * sampleRate;
    this.underruns = 0;
    this.played = 0;
    this.received = 0;
    this.sinceReport = 0;
    const onData = ({ data }) => {
      if (data instanceof Float32Array) {
        this.queue.push(data);
        this.queued += data.length / 2;
        this.received += data.length / 2;
      } else if (data && data.type === 'config') {
        this.start = Math.max(0, data.startSeconds) * sampleRate;
      } else if (data && data.type === 'flush') {
        this.queue = []; this.head = 0; this.queued = 0; this.playing = false;
      }
    };
    this.port.onmessage = ({ data }) => {
      // The emulator worker's PCM port arrives here.
      if (data && data.port) data.port.onmessage = onData;
      else onData({ data });
    };
  }
  process(_inputs, outputs) {
    const out = outputs[0];
    const left = out[0];
    const right = out[1] ?? out[0];
    const n = left.length;
    if (!this.playing && this.queued > 0 && this.queued >= this.start) this.playing = true;
    let i = 0;
    if (this.playing) {
      while (i < n && this.queue.length) {
        const block = this.queue[0];
        left[i] = block[this.head];
        right[i] = block[this.head + 1];
        i += 1;
        this.head += 2;
        if (this.head >= block.length) { this.queue.shift(); this.head = 0; }
      }
      this.queued -= i;
      this.played += i;
      if (i < n) { this.playing = false; this.underruns += 1; }
    }
    for (; i < n; i += 1) { left[i] = 0; right[i] = 0; }
    this.sinceReport += n;
    if (this.sinceReport >= sampleRate / 4) {
      this.sinceReport = 0;
      this.port.postMessage({
        queuedSeconds: this.queued / sampleRate, playing: this.playing,
        underruns: this.underruns, playedSeconds: this.played / sampleRate,
        receivedSeconds: this.received / sampleRate,
      });
    }
    return true;
  }
}
registerProcessor('pcm-player', PcmProcessor);
