/** Browser PCM sink for the coupled core: AudioContext + AudioWorklet. */
export interface AudioStats { queuedSeconds: number; playing: boolean; underruns: number; playedSeconds: number; receivedSeconds: number }

export class PcmSink {
  readonly context: AudioContext;
  private node?: AudioWorkletNode;
  /** Latest worklet report. */
  stats: AudioStats = { queuedSeconds: 0, playing: false, underruns: 0, playedSeconds: 0, receivedSeconds: 0 };

  /** Construct from a user gesture. 48 kHz is the DSP rate; the browser resamples to the device. */
  constructor() { this.context = new AudioContext({ sampleRate: 48_000, latencyHint: 'playback' }); }

  /** Loads the worklet and returns the port the emulator worker posts PCM to. */
  async open(startSeconds: number): Promise<MessagePort> {
    await this.context.audioWorklet.addModule('/pcm-worklet.js');
    this.node = new AudioWorkletNode(this.context, 'pcm-player', { numberOfInputs: 0, outputChannelCount: [2] });
    this.node.connect(this.context.destination);
    this.node.port.onmessage = ({ data }: MessageEvent<AudioStats>) => { this.stats = data; };
    this.setBuffer(startSeconds);
    const channel = new MessageChannel();
    this.node.port.postMessage({ port: channel.port2 }, [channel.port2]);
    await this.context.resume();
    return channel.port1;
  }

  /** Start playing once this much audio is queued (0: immediately). */
  setBuffer(seconds: number) { this.node?.port.postMessage({ type: 'config', startSeconds: seconds }); }
  close() { this.node?.disconnect(); void this.context.close(); }
}
