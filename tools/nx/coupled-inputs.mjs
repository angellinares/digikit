// Prints one sha256 hex line over the untracked inputs of the coupled-run FX targets (gate-fx-dn2, gate-fx-dt2):
// the firmware and the private ready inputs that `tools/emulator/coupled.py` boots from.
// They are not the patchkit ones that tools/nx/device-inputs.mjs hashes. Mirrors tools/emulator/digi_env.py
// (`firmware`, `firmware_for`, `READY_DIRS`, `ready_inputs`, `ready_inputs.json`); a change there must change this file.
// The DT2 FX card is built from a generated sine, so it has no input file; DIGI_DT2_CARD (a kept card image) is hashed.
import { createHash } from 'node:crypto';
import { createReadStream, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const readyJson = JSON.parse(readFileSync(join(root, 'tools/emulator/ready_inputs.json'), 'utf8'));

const fileHash = (file) =>
  new Promise((done) => {
    const h = createHash('sha256');
    const s = createReadStream(file);
    s.on('data', (c) => h.update(c));
    s.on('end', () => done(h.digest('hex')));
    s.on('error', () => done('missing'));
  });

const isFile = (p) => {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
};

// A bare environment value is relative to the repository root, as in digi_env.py.
const under = (p) => resolve(root, p);

export const coupledInputs = async (env = process.env) => {
  const lines = [];
  const add = async (label, file) => lines.push(`${label} ${isFile(file) ? await fileHash(file) : 'missing'}`);

  await add('dn2/syx', env.DIGI_EMU_SYX || env.DIGI_COUPLED_SYX || join(root, 'firmware/Digitone_II_OS1.12.syx'));
  await add('dt2/syx', env.DIGI_EMU_SYX_DT2 || join(root, 'firmware/Digitakt_II_OS1.17.syx'));

  const dn2Ready = under(env.DIGI_EMU_READY_DIR || readyJson.ready_dir);
  const dt2Ready = under(env.DIGI_EMU_READY_DIR_DT2 || readyJson.ready_dirs.dt2);
  await add('dn2/ready/dsp-image', env.DIGI_EMU_DSP_IMAGE || join(dn2Ready, 'digi-audio-dn2-image.bin'));
  await add('dn2/ready/dsp-state', env.DIGI_EMU_DSP_STATE || join(dn2Ready, 'digi-audio-m5.snap.dsp'));
  await add('dn2/ready/cf-snapshot', env.DIGI_EMU_CF_SNAPSHOT || join(dn2Ready, 'digi-audio-m5.snap'));
  for (const f of ['digi-audio-dn2-image.bin', 'digi-audio-m5.snap.dsp', 'digi-audio-m5.snap']) {
    await add(`dt2/ready/${f}`, join(dt2Ready, f));
  }
  if (env.DIGI_DT2_CARD) await add('dt2/card', under(env.DIGI_DT2_CARD));
  return createHash('sha256').update(`coupled-inputs v1\n${lines.join('\n')}`).digest('hex');
};

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) console.log(await coupledInputs());
