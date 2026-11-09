import fs from 'node:fs';
import path from 'node:path';
import { cardCacheRoot, repoRoot, type Device } from '@digi/test-support';
import { claimDir, newestFirst, pruneKeyDirs, type RunDirClaim } from './run-claim.ts';

export const DEVICES = ['dt2', 'dn2'] as const;
export const FIRMWARE = { dt2: '1.17', dn2: '1.12' } as const;

const claimedRunDirs = new Map<Device, string>();

/** The daemon build key: the parent directory of a binary in the shared `<git dir>/emud-cache/<dev>/<key>/digi-emud` store. A custom binary has none. */
export function daemonKey(binary: string): string | undefined {
  const parts = path.resolve(binary).split(path.sep);
  const at = parts.lastIndexOf('emud-cache');
  return at >= 0 && parts.length === at + 4 ? parts[at + 2] : undefined;
}

/** Card dirs kept in the card cache (newest by mtime). */
const CARDS_KEEP = 32;
/** A card dir touched within this time is never evicted: another worker may have loaded it. */
const CARDS_MIN_AGE_MS = 120 * 60_000;
/** A work or temp dir of a card build older than this is a leftover of a killed run. */
const WORK_MAX_AGE_MS = 60 * 60_000;

/** Evict card dirs of the card cache beyond the newest CARDS_KEEP (cards are touched on reuse), and the leftovers of killed builds. */
export function evictCards(): void {
  const root = cardCacheRoot();
  if (!fs.existsSync(root)) return;
  for (const entry of newestFirst(root).slice(CARDS_KEEP))
    if (Date.now() - entry.mtime > CARDS_MIN_AGE_MS) fs.rmSync(entry.full, { recursive: true, force: true });
  for (const name of fs.readdirSync(root).filter((n) => n.startsWith('.work-'))) {
    const full = path.join(root, name);
    try {
      if (Date.now() - fs.statSync(full).mtimeMs > WORK_MAX_AGE_MS) fs.rmSync(full, { recursive: true, force: true });
    } catch {
      /* removed meanwhile */
    }
  }
}

function sessionBase(device: Device): string | undefined {
  const key = daemonKey(daemonBinary(device));
  return key ? path.join(repoRoot(), 'dist/session-cache', key.slice(0, 12)) : undefined;
}

/** One run dir per device and vitest worker process at a time, so no two daemons share it. Dirs are reused
 * across processes (a pid lock file marks a dir as claimed), so the booted-snapshot cache stays warm: a cold
 * boot once per dir, then about 1 s. The dir count is bounded by the number of concurrent workers.
 * With a daemon from the shared build store the dirs sit below the daemon's build key
 * (`dist/session-cache/<key12>/<device>-<n>`): snapshots carry no version, so a new daemon build must not
 * load those of an older one. Keeps the newest PATCHED_KEEP snapshot entries. Used by `liveOptions` (not wiped). */
export function cachedRunDir(device: Device): string {
  const known = claimedRunDirs.get(device);
  if (known) return known;
  const claim = claimDir(sessionBase(device) ?? path.join(repoRoot(), 'dist/session-cache'), device, false);
  claimedRunDirs.set(device, claim.dir);
  return claim.dir;
}

/**
 * The run dir of one daemon start of a runtime fixture that wants snapshot restores: a persistent dir below the daemon's build key,
 * wiped of everything but the daemon's caches. Release it when the session has closed (`failed`: the dir is renamed and not reused).
 * Without a build key (a custom `DIGI_EMUD_<DEV>`) it gives undefined: the session uses a fresh temporary dir.
 */
export function claimRunDir(device: Device): RunDirClaim | undefined {
  const base = sessionBase(device);
  if (!base) return undefined;
  const claim = claimDir(base, device, true);
  pruneKeyDirs(path.dirname(base));
  evictCards();
  return claim;
}

/** The device daemon: `DIGI_EMUD_<DEV>`, else the same key in `dist/daemons.json` (written by `digi-control:daemons`). */
export function daemonBinary(device: Device): string {
  const key = `DIGI_EMUD_${device.toUpperCase()}`;
  const fromEnv = process.env[key];
  if (fromEnv) return fromEnv;
  const file = path.join(repoRoot(), 'dist/daemons.json');
  if (fs.existsSync(file)) {
    const value = (JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, string>)[key];
    if (value) return value;
  }
  throw new Error(`${key} unset and dist/daemons.json missing: run pnpm nx:pilot run digi-control:daemons`);
}

/** Mirror sessions and `fresh: true` keep a fresh run dir (the mirror folder must start empty). */
export function liveOptions(device: Device, extra: { patchSet?: string; mirror?: boolean; fresh?: boolean } = {}) {
  const binary = daemonBinary(device);
  const card = device === 'dt2' ? path.join(repoRoot(), 'out/runs/patchkit/samples') : undefined;
  if (card && !fs.existsSync(card)) {
    throw new Error(
      `DT2 sample card missing: generate ${card} (tools.emulator.card_check.make_samples, run by pk_harness)`,
    );
  }
  const { fresh, ...rest } = extra;
  const runDir = extra.mirror || fresh ? undefined : cachedRunDir(device);
  return { device, firmware: FIRMWARE[device], binary, card, ...rest, runDir };
}

/** Full-tier scenarios run only under `DIGIKIT_TIER=full` (`verify --full`), as the Python tiers do. */
export const FULL_TIER = process.env.DIGIKIT_TIER === 'full';
