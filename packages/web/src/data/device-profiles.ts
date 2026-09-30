import { parse } from 'smol-toml';
import dt2Source from '../../../../devices/digitakt-ii.toml?raw';
import dn2Source from '../../../../devices/digitone-ii.toml?raw';

export type DeviceId = 'dt2' | 'dn2';
export type BootContract = 'main-os-oracle-v1';
export type SymbolProfile = 'elektron-rtos-v1';
export type FirmwareIdentity = Readonly<{ version: string; sha256: string; filename?: string; main_sha256?: string; boot_contract?: BootContract; symbol_profile?: SymbolProfile }>;
export type DeviceProfile = Readonly<{ name: string; short: DeviceId; firmware: readonly FirmwareIdentity[] }>;

type ParsedDocument = { device?: { name?: unknown; short?: unknown }; firmware?: unknown };
type ParsedFirmware = { version?: unknown; sha256?: unknown; filename?: unknown; main_sha256?: unknown; boot_contract?: unknown; symbol_profile?: unknown };
const HEX = /^[0-9a-f]{64}$/i;

function string(value: unknown, field: string): string {
  if (typeof value !== 'string' || value.length === 0) throw new Error(`device profile ${field} must be a non-empty string`);
  return value;
}

function hash(value: unknown, field: string): string {
  const result = string(value, field).toLowerCase();
  if (!HEX.test(result)) throw new Error(`device profile ${field} must be a SHA-256 hex digest`);
  return result;
}

function profile(source: string, expected: DeviceId): DeviceProfile {
  const parsed = parse(source) as ParsedDocument;
  const name = string(parsed.device?.name, 'device.name');
  const short = string(parsed.device?.short, 'device.short');
  if (short !== expected) throw new Error(`device profile device.short must be ${expected}`);
  if (!Array.isArray(parsed.firmware)) throw new Error('device profile firmware must be an array');
  const firmware = parsed.firmware.map((entry): FirmwareIdentity => {
    if (entry === null || typeof entry !== 'object') throw new Error('device profile firmware entry must be a table');
    const raw = entry as ParsedFirmware;
    const main = raw.main_sha256;
    const contract = raw.boot_contract;
    const symbols = raw.symbol_profile;
    const hasBoot = main !== undefined || contract !== undefined || symbols !== undefined;
    if (hasBoot && (main === undefined || contract !== 'main-os-oracle-v1' || symbols !== 'elektron-rtos-v1')) throw new Error('device profile boot fields must be a supported complete contract');
    return {
      version: string(raw.version, 'firmware.version'), sha256: hash(raw.sha256, 'firmware.sha256'),
      ...(raw.filename === undefined ? {} : { filename: string(raw.filename, 'firmware.filename') }),
      ...(hasBoot ? { main_sha256: hash(main, 'firmware.main_sha256'), boot_contract: contract, symbol_profile: symbols } : {}),
    };
  });
  return { name, short: expected, firmware };
}

export const deviceProfiles = { dt2: profile(dt2Source, 'dt2'), dn2: profile(dn2Source, 'dn2') } as const;

const allHashes = new Set<string>();
for (const device of Object.values(deviceProfiles)) for (const firmware of device.firmware) {
  if (allHashes.has(firmware.sha256)) throw new Error(`duplicate firmware SHA-256 ${firmware.sha256}`);
  allHashes.add(firmware.sha256);
}
