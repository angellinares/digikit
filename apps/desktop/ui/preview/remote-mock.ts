// The Remote page and the Emulator page's faceplate in the preview (`?page=remote`, `?page=emulator`): the device screen is a test
// pattern, so a screenshot shows where and how large the 128 x 64 image is drawn. The Remote page is the app's own code; only the
// DkSession methods that ask the device are replaced.
import { DkSession } from '@digi/dk-session';
import { queryFrame } from './frame-from-png';

/** A recognisable 128 x 64 frame in the firmware's screen layout (`screenPixel`): a frame, corner marks, a diagonal, and a bar of steps. */
export function testPattern(): Uint8Array {
  const buf = new Uint8Array(1024);
  const set = (x: number, y: number) => {
    if (x < 0 || x > 127 || y < 0 || y > 63) return;
    buf[7 - (y >> 3) + 8 * x] |= 1 << (y & 7);
  };
  for (let x = 0; x < 128; x++) {
    set(x, 0);
    set(x, 63);
  }
  for (let y = 0; y < 64; y++) {
    set(0, y);
    set(127, y);
  }
  for (let i = 0; i < 64; i++) set(i * 2, i); // diagonal corner to corner
  for (let i = 0; i < 64; i++) set(127 - i * 2, i);
  for (let k = 0; k < 8; k++)
    for (let y = 56 - k * 2; y < 60; y++) for (let x = 8 + k * 4; x < 11 + k * 4; x++) set(x, y);
  // a letter F in the upper left (5 x 7 at 3x), so a mirror or a rotation is visible
  for (let y = 0; y < 21; y++) set(8, 8 + y);
  for (let x = 0; x < 12; x++) {
    set(8 + x, 8);
    set(8 + x, 18);
  }
  return buf;
}

export function installRemoteMock(): void {
  const proto = DkSession.prototype as unknown as Record<string, unknown>;
  const fallback = testPattern();
  const loaded = queryFrame();
  proto.screen = async () => ((await loaded) ?? fallback).slice();
  proto.info = async () => ({ port: 'dn2-1.12' });
  proto.remoteState = async () => ({ enabled: true, format: 1, turnLimit: 10 });
  proto.remoteKey = async () => {};
  proto.remoteTurn = async () => {};
  proto.remoteRelease = async () => {};
}
