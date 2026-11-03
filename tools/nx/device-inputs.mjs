// Prints one sha256 hex line over the untracked inputs of the device tests: firmware images, ready dirs
// and the DT2 sample card, plus binary overrides (root overrides deliberately prevent reuse).
// Mirrors packages/test-support/src/inputs.ts (FIRMWARE table, ready files,
// DIGI_PK_READY_<DEV>_<VER> override) and tests/regression/runtime/devices.ts (DT2 card dir
// out/runs/patchkit/samples); a change there must change this file.
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

const CELLS = [
  { dev: 'dt2', ver: '1.17', syx: 'firmware/Digitakt_II_OS1.17.syx', ready: 'out/ready-pk-dt2-117', samples: true },
  { dev: 'dn2', ver: '1.12', syx: 'firmware/Digitone_II_OS1.12.syx', ready: 'out/ready-fix-dn2-112', samples: false },
];
const READY_FILES = ['digi-audio-m5.snap', 'digi-audio-dn2-image.bin', 'digi-audio-m5.snap.dsp', 'SOURCE.json'];

const fileHash = (file) =>
  new Promise((done) => {
    const h = createHash('sha256');
    const s = createReadStream(file);
    s.on('data', (c) => h.update(c));
    s.on('end', () => done(h.digest('hex')));
    s.on('error', () => done('missing'));
  });

const filesUnder = (dir) => {
  const out = [];
  const walk = (d) => {
    let entries;
    try {
      entries = readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) out.push(p);
    }
  };
  walk(dir);
  return out.map((p) => relative(dir, p)).sort();
};

const isFile = (p) => {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
};

// These binaries bypass prerequisite outputs. Hash bytes as well as the spelling of the path:
// an override can be replaced in place, and stat/read streams follow binary symlinks.
export const overrideInputs = async (env = process.env) => {
  const lines = [];
  for (const key of [
    'DIGI_EMUD',
    'DIGI_EMUD_DT2',
    'DIGI_EMUD_DN2',
    'DIGI_PATCHKIT',
    'DIGI_PATCHKIT_BIN',
    'DIGI_PLUSDRIVE',
    'DIGI_STORAGE_FAULT_EMUD',
    'DIGI_LP0_EMUD',
    'DIGI_DSP_PEEK_EMUD',
    'DIGI_DDR_MONITOR_EMUD',
    'DIGI_PATCHED_STARTUP_EMUD',
    'DIGI_STEREO_PROOF_EMUD',
    'DIGI_CATALOG_EMUD',
    'DIGI_FILES_EMUD_DT2',
    'DIGI_FILES_EMUD_DN2',
  ]) {
    const value = env[key];
    lines.push(`${key} ${JSON.stringify(value)}`);
    if (value) {
      // Do not open FIFOs/devices, which could hang the Nx runtime input.
      lines.push(`${key}/bytes ${isFile(value) ? await fileHash(value) : 'missing-or-not-file'}`);
      if (isFile(value)) lines.push(`${key}/mode ${statSync(value).mode & 0o777}`);
    }
  }
  for (const key of ['PATCHKIT_MODULES', 'DIGI_PK_ROOT', 'DIGI_RUNTIME_MODULES']) {
    const value = env[key];
    lines.push(`${key} ${JSON.stringify(value)}`);
    // Arbitrary module roots can read sources/includes outside their tree. Variant roots also
    // supply mutable out/cache/dko and out/tools. DIGI_RUNTIME_MODULES supplies external artifacts.
    // There is no bounded, proven read closure:
    // keep overrides supported, but refuse cache reuse (including missing/symlinked roots).
    if (value || (key === 'DIGI_RUNTIME_MODULES' && value !== undefined)) lines.push(`${key}/uncached ${randomUUID()}`);
  }
  // Specialized acceptance fixtures include cards, patched images/plans and provenance JSON
  // referring to further ready files. Avoid reusing a result without their transitive read closure.
  for (const key of [
    'DIGI_CATALOG_CARD',
    'DIGI_FILES_CARD_DT2',
    'DIGI_FILES_PATCH_DT2',
    'DIGI_FILES_PATCH_DN2',
    'DIGI_FILES_PLAN_DT2',
    'DIGI_FILES_PLAN_DN2',
    'DIGI_LP0_BOOT_FIXTURE',
  ]) {
    lines.push(`${key} ${JSON.stringify(env[key])}`);
    if (env[key] !== undefined) lines.push(`${key}/uncached ${randomUUID()}`);
  }
  for (const key of ['DIGI_LP0_VOICES', 'DIGI_LP0_CLOSE', 'DIGI_LP0_AUDIO', 'DIGI_LP0_FULL_BANK']) {
    lines.push(`${key} ${JSON.stringify(env[key])}`);
  }
  return lines;
};

export const deviceInputs = async () => {
  const lines = [];
  for (const c of CELLS) {
    const label = `${c.dev}-${c.ver}`;
    const readyDir =
      process.env[`DIGI_PK_READY_${c.dev.toUpperCase()}_${c.ver.replace('.', '')}`] ?? join(root, c.ready);
    lines.push(`${label}/syx ${await fileHash(join(root, c.syx))}`);
    for (const f of READY_FILES) lines.push(`${label}/ready/${f} ${await fileHash(join(readyDir, f))}`);
    if (c.samples) {
      const dir = join(root, 'out/runs/patchkit/samples');
      for (const rel of filesUnder(dir)) {
        if (isFile(join(dir, rel))) lines.push(`${c.dev}/samples/${rel} ${await fileHash(join(dir, rel))}`);
      }
    }
  }

  return createHash('sha256')
    .update(`device-inputs v2\n${[...lines, ...(await overrideInputs())].join('\n')}`)
    .digest('hex');
};

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) console.log(await deviceInputs());
