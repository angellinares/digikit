/** Future worker contract. This shell deliberately does not implement it. */
import { deviceProfiles, type DeviceId } from './data/device-profiles';

export interface EmulatorRuntime {
  press(code: number): void;
  release(code: number): void;
  turn(encoder: number, detents: number): void;
}

export const knownFirmware: Record<DeviceId, readonly (readonly [string, string])[]> = {
  dt2: deviceProfiles.dt2.firmware.map(({ version, sha256 }) => [version, sha256]),
  dn2: deviceProfiles.dn2.firmware.map(({ version, sha256 }) => [version, sha256]),
};

export async function identifyFirmware(file: File, device: keyof typeof knownFirmware) {
  const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
  const hash = [...new Uint8Array(digest)].map((x) => x.toString(16).padStart(2, '0')).join('');
  const found = knownFirmware[device].find(([, expected]) => expected === hash);
  return found ? { version: found[0], hash } : null;
}
