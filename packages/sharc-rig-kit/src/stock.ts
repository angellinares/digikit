/**
 * The stock DSP image as the base core of a rig: firmware section 7 (an ADI loader stream) read at run time from `out/sections/<id>/`,
 * never copied into a fixture. A test that needs it skips with `STOCK_SKIP` text when the firmware was not extracted.
 */
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { ROOT } from './rig.ts';

export interface StockImage {
  id: string;
  /** sha256 of section 7 (a different image is a different test) */
  sha256: string;
}

/** DN2 OS 1.12 */
export const DN2_112: StockImage = {
  id: 'dn2-1.12',
  sha256: '336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2',
};

export const stockPath = (image: StockImage): string => path.join(ROOT, 'out/sections', image.id, 'section_7_BLOB.bin');

export const stockAvailable = (image: StockImage): boolean => existsSync(stockPath(image));

/** Why a test that needs the image is skipped (a describe title). */
export const stockSkipReason = (image: StockImage): string =>
  `requires the original ${image.id} section 7 (${stockPath(image)})`;

const loaded = new Map<string, string>();

/** The image's hex for `RigSetup.image`; the bytes are checked against the pinned hash. */
export function stockHex(image: StockImage): string {
  const known = loaded.get(image.id);
  if (known) return known;
  const bytes = readFileSync(stockPath(image));
  const hash = createHash('sha256').update(bytes).digest('hex');
  if (hash !== image.sha256) throw new Error(`${stockPath(image)} is not ${image.id} (sha256 ${hash})`);
  const hex = bytes.toString('hex');
  loaded.set(image.id, hex);
  return hex;
}
