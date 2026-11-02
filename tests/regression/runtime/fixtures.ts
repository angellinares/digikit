import type { Device } from '@digi/test-support';

/** Module ids of the test fixtures and the runtime root modules shared by the FX scenarios. */
export const SINE = 'test/sine';
export const GAIN = 'test/dyn-fx-gain';
export const SEND = 'test/dyn-fx-gain-send';
export const FX = 'test/dyn-fx-gain-both';
export const RT = ['core/runtime', 'core/dynreg', 'core/dyndsp'];

/** The absolute DSP frame of the note trigger that every FX scenario shares per device. */
export const triggerFrame = (device: Device): number => (device === 'dt2' ? 7500 : 7000);

/** 0.1 seconds at 44.1 kHz exercises the converter's resampling path. */
export function sampleWav(channels: 1 | 2): Uint8Array {
  const frames = 4410;
  const pcm = Buffer.alloc(frames * channels * 2);
  for (let i = 0; i < frames; i++) {
    pcm.writeInt16LE(Math.round(12000 * Math.sin((2 * Math.PI * 440 * i) / 44100)), i * channels * 2);
    if (channels === 2) pcm.writeInt16LE(Math.round(9000 * Math.sin((2 * Math.PI * 660 * i) / 44100)), i * 4 + 2);
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + pcm.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(1, 20);
  h.writeUInt16LE(channels, 22);
  h.writeUInt32LE(44100, 24);
  h.writeUInt32LE(44100 * channels * 2, 28);
  h.writeUInt16LE(channels * 2, 32);
  h.writeUInt16LE(16, 34);
  h.write('data', 36);
  h.writeUInt32LE(pcm.length, 40);
  return Uint8Array.from(Buffer.concat([h, pcm]));
}
