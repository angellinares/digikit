// Prints one sha256 hex line over the untracked inputs of the device tests: firmware images, ready dirs
// and the DT2 sample card. Mirrors packages/test-support/src/inputs.ts (FIRMWARE table, ready files,
// DIGI_PK_READY_<DEV>_<VER> override) and tests/regression/runtime/devices.ts (DT2 card dir
// out/runs/patchkit/samples); a change there must change this file.
import { createHash } from 'node:crypto';
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

const lines = [];
for (const c of CELLS) {
  const label = `${c.dev}-${c.ver}`;
  const readyDir = process.env[`DIGI_PK_READY_${c.dev.toUpperCase()}_${c.ver.replace('.', '')}`] ?? join(root, c.ready);
  lines.push(`${label}/syx ${await fileHash(join(root, c.syx))}`);
  for (const f of READY_FILES) lines.push(`${label}/ready/${f} ${await fileHash(join(readyDir, f))}`);
  if (c.samples) {
    const dir = join(root, 'out/runs/patchkit/samples');
    for (const rel of filesUnder(dir)) {
      if (isFile(join(dir, rel))) lines.push(`${c.dev}/samples/${rel} ${await fileHash(join(dir, rel))}`);
    }
  }
}

console.log(
  createHash('sha256')
    .update(`device-inputs v1\n${lines.join('\n')}`)
    .digest('hex'),
);
