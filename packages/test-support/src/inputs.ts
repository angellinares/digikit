import fs from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export type Device = 'dt2' | 'dn2';

// (device, version) -> base .syx and ready dir; same table as FIRMWARE in tools/patchkit/pk_harness.py.
// Override the ready dir with DIGI_PK_READY_<DEV>_<VER> (version without the dot).
const FIRMWARE: Record<string, { syx: string; ready: string }> = {
  'dt2 1.17': { syx: 'Digitakt_II_OS1.17.syx', ready: 'out/ready-pk-dt2-117' },
  'dn2 1.12': { syx: 'Digitone_II_OS1.12.syx', ready: 'out/ready-fix-dn2-112' },
};

export interface ReadyInputs {
  syx: string;
  readyDir: string;
  cfSnapshot: string;
  dspImage: string;
  dspState: string;
}

export function repoRoot(): string {
  let dir = path.dirname(fileURLToPath(import.meta.url));
  for (;;) {
    if (fs.existsSync(path.join(dir, 'pnpm-workspace.yaml'))) return dir;
    const up = path.dirname(dir);
    if (up === dir) throw new Error('repo root (pnpm-workspace.yaml) not found');
    dir = up;
  }
}

/** The .syx and ready-input files of a device and firmware version; throws when one is missing. */
export function resolveInputs(device: Device, firmware: string): ReadyInputs {
  const root = repoRoot();
  const spec = FIRMWARE[`${device} ${firmware}`];
  let syx: string;
  let readyDir: string;
  if (spec) {
    syx = path.join(root, 'firmware', spec.syx);
    const env = process.env[`DIGI_PK_READY_${device.toUpperCase()}_${firmware.replace('.', '')}`];
    readyDir = env ?? path.join(root, spec.ready);
  } else {
    throw new Error(`no firmware entry for ${device} ${firmware}`);
  }
  const ready = {
    syx,
    readyDir,
    cfSnapshot: path.join(readyDir, 'digi-audio-m5.snap'),
    dspImage: path.join(readyDir, 'digi-audio-dn2-image.bin'),
    dspState: path.join(readyDir, 'digi-audio-m5.snap.dsp'),
  };
  for (const f of [ready.syx, ready.cfSnapshot, ready.dspImage, ready.dspState]) {
    if (!fs.existsSync(f)) {
      throw new Error(
        `${f} is missing; build the ready dir with ` +
          `\`uv run python -m tools.emulator.ready ${device} --syx ${path.basename(syx)} --out ${readyDir} --force\``,
      );
    }
  }
  return ready;
}

/** Exact offline artifact identity; modules are the prepared plan's complete dependency closure. */
export interface PreparedRuntimeImage {
  syx: string;
  baseSha256: string;
  syxSha256: string;
  modules: string[];
}

export function validatePreparedRuntimeImage(image: PreparedRuntimeImage, stockSyx: string): void {
  const digest = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  if (
    !/^[a-f0-9]{64}$/.test(image.baseSha256) ||
    !/^[a-f0-9]{64}$/.test(image.syxSha256) ||
    digest(stockSyx) !== image.baseSha256 ||
    digest(image.syx) !== image.syxSha256 ||
    image.modules.length === 0 ||
    new Set(image.modules).size !== image.modules.length
  ) {
    throw new Error('prepared runtime image does not match its stock/artifact hashes or module membership');
  }
}

/** Runtime boots require ready inputs from this stock image, without forced interrupt state. */
export function validateRuntimeInputs(inputs: ReadyInputs): void {
  const source = JSON.parse(fs.readFileSync(path.join(inputs.readyDir, 'SOURCE.json'), 'utf8'));
  const sha256 = createHash('sha256').update(fs.readFileSync(inputs.syx)).digest('hex');
  if (source.syx_sha256 !== sha256 || source.intc_force !== false) {
    throw new Error(`${inputs.readyDir}: runtime ready provenance mismatch (stock SHA-256 or intc_force)`);
  }
}

/** Match the coupled launcher's verified fast-tier defaults for the actual stock bytes.
 * Explicit values (including empty strings) win; DIGI_EMU_FAST=0 suppresses defaults.
 * The daemon inherits process.env, so disabled defaults do not erase explicit settings. */
export function coupledEnv(syx: string, environ: NodeJS.ProcessEnv = process.env): Record<string, string> {
  if (environ.DIGI_EMU_FAST === '0') return {};
  const digest = createHash('sha256').update(fs.readFileSync(syx)).digest('hex');
  const table: Record<string, { fast_regions?: string; blocktrans?: string; fallback_regions?: string }> = JSON.parse(
    fs.readFileSync(path.join(repoRoot(), 'config/emulator.json'), 'utf8'),
  );
  const entry = table[digest];
  const defaults = {
    SHARC_FAST_REGIONS: entry?.fast_regions,
    SHARC_BLOCKTRANS: entry?.blocktrans,
    // Explicit ordinary regions or auto selection retain control of these roots.
    SHARC_FAST_FALLBACK_REGIONS:
      environ.SHARC_FAST_REGIONS === undefined && environ.SHARC_FAST_AUTO === undefined
        ? entry?.fallback_regions
        : undefined,
  };
  const result: Record<string, string> = {};
  for (const [name, value] of Object.entries(defaults)) {
    if (environ[name] !== undefined) result[name] = environ[name];
    else if (value) result[name] = value;
  }
  return result;
}
