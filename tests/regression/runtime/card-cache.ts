import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { cardCacheRoot, repoRoot, type Device } from '@digi/test-support';
import { plusdriveBinary } from './card-kit.ts';

/**
 * Content-named card images. The daemon's boot snapshot key holds the card's path, size and mtime and the sha256 of its `overlay.bin`, so
 * a card that is rebuilt into a new directory never matches an earlier snapshot. A card built here lives in
 * `dist/session-cache/cards/<key>/`, where the key is a hash of everything that goes into the card: the spec, the bytes of every input
 * file, the plusdrive binary, the device profiles and a format version. The same inputs give the same path, size and mtime, so the
 * snapshot of an earlier run is found. The files are read-only: a write to a shared card fails.
 */

/** Change it when the way a card is built changes in a way that the other key parts do not show. */
const FORMAT = 'card-cache-1';

const shaCache = new Map<string, { sig: string; sha: string }>();

/** The sha256 (hex) of a file, read once per path, size and mtime. */
export function fileSha(file: string): string {
  const stat = fs.statSync(file);
  const sig = `${stat.size}:${stat.mtimeMs}`;
  const known = shaCache.get(file);
  if (known?.sig === sig) return known.sha;
  const hash = createHash('sha256');
  const fd = fs.openSync(file, 'r');
  try {
    const buf = Buffer.alloc(1 << 20);
    for (let n; (n = fs.readSync(fd, buf, 0, buf.length, null)) > 0;) hash.update(buf.subarray(0, n));
  } finally {
    fs.closeSync(fd);
  }
  const sha = hash.digest('hex');
  shaCache.set(file, { sig, sha });
  return sha;
}

/** JSON with sorted object keys; bytes become their sha256. */
function canonical(value: unknown): string {
  return JSON.stringify(value, function (this: unknown, key: string, v: unknown) {
    const raw = (this as Record<string, unknown>)[key];
    if (raw instanceof Uint8Array) return { bytes: createHash('sha256').update(raw).digest('hex'), length: raw.length };
    if (v && typeof v === 'object' && !Array.isArray(v))
      return Object.fromEntries(Object.entries(v).sort(([a], [b]) => (a < b ? -1 : 1)));
    return v;
  });
}

/** The tools that build a card: the plusdrive binary and the device profiles it reads (every .toml file below devices/). */
function toolIdentity(): { plusdrive: string; profiles: Record<string, string> } {
  const dir = path.join(repoRoot(), 'devices');
  const profiles: Record<string, string> = {};
  // Every .toml file below devices/: the profiles and the firmware ports below devices/ports/.
  for (const name of fs.readdirSync(dir, { recursive: true, encoding: 'utf8' }).sort())
    if (name.endsWith('.toml')) profiles[name] = fileSha(path.join(dir, name));
  return { plusdrive: fileSha(plusdriveBinary()), profiles };
}

/** The key of a card: `spec` is JSON-like, with each input file given as its sha256 (`fileSha`) and bytes as `Uint8Array`. */
export function cardKey(device: Device, spec: unknown): string {
  return createHash('sha256')
    .update(canonical({ format: FORMAT, device, spec, tools: toolIdentity() }))
    .digest('hex')
    .slice(0, 32);
}

/** The key of a cached card, or undefined for a card outside the cache (a card that a test or the guest wrote). */
export function cachedCardId(card: string): { key: string; overlaySha: string } | undefined {
  const dir = path.dirname(card);
  if (path.dirname(dir) !== cardCacheRoot()) return undefined;
  const overlay = path.join(dir, 'overlay.bin');
  return { key: path.basename(dir), overlaySha: fs.existsSync(overlay) ? fileSha(overlay) : '' };
}

/**
 * The card for KEY. When its directory exists it is reused as it is (and touched, so eviction keeps it). Otherwise `produce` builds it in
 * a work directory and returns the `card.img` it made; the card's directory (`card.img` and `overlay.bin`) is made read-only and renamed
 * into place. Two workers that build the same key at once are safe: the one that loses the rename uses the winner's card.
 */
export async function cachedCard(key: string, produce: (work: string) => Promise<string>): Promise<string> {
  const root = cardCacheRoot();
  const final = path.join(root, key);
  const card = path.join(final, 'card.img');
  if (fs.existsSync(card)) {
    const now = new Date();
    fs.utimesSync(final, now, now);
    return card;
  }
  fs.mkdirSync(root, { recursive: true });
  const work = fs.mkdtempSync(path.join(root, '.work-'));
  try {
    const made = await produce(work);
    for (const name of ['card.img', 'overlay.bin']) {
      const file = path.join(path.dirname(made), name);
      if (fs.existsSync(file)) fs.chmodSync(file, 0o444);
    }
    try {
      fs.renameSync(path.dirname(made), final);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code !== 'ENOTEMPTY' && code !== 'EEXIST') throw error;
    }
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
  return card;
}
