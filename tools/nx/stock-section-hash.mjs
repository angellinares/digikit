// Nx runtime input of the `test-rig` targets: the sha256 of the stock DSP section the stereo rig tests load (the file
// packages/sharc-rig-kit/src/stock.ts reads), or "absent". Without it a run that skipped the stock-image suites could be replayed
// by a clone that has the firmware. DIGI_STOCK_SECTION_DIR points at another out/sections root (proof only).
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const IMAGE = 'dn2-1.12';

export const stockSectionHash = (env = process.env) => {
  const root = env.DIGI_STOCK_SECTION_DIR ?? fileURLToPath(new URL('../../out/sections', import.meta.url));
  const file = resolve(root, IMAGE, 'section_7_BLOB.bin');
  return existsSync(file) ? createHash('sha256').update(readFileSync(file)).digest('hex') : 'absent';
};

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) console.log(stockSectionHash());
