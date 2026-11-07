import fs from 'node:fs';
import path from 'node:path';
import { cardCacheRoot, repoRoot, type Device } from '@digi/test-support';

export const DEVICES = ['dt2', 'dn2'] as const;
export const FIRMWARE = { dt2: '1.17', dn2: '1.12' } as const;

const claimedRunDirs = new Map<Device, string>();

function tryLock(lock: string): boolean {
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = fs.openSync(lock, 'wx');
      fs.writeSync(fd, String(process.pid));
      fs.closeSync(fd);
      return true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    }
    const pid = Number.parseInt(fs.readFileSync(lock, 'utf8'), 10);
    if (Number.isInteger(pid) && pid > 0) {
      try {
        process.kill(pid, 0);
        return false;
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH') return false;
      }
    }
    fs.rmSync(lock, { force: true });
  }
  return false;
}

/** The daemon build key: the parent directory of a binary in the shared `<git dir>/emud-cache/<dev>/<key>/digi-emud` store. A custom binary has none. */
export function daemonKey(binary: string): string | undefined {
  const parts = path.resolve(binary).split(path.sep);
  const at = parts.lastIndexOf('emud-cache');
  return at >= 0 && parts.length === at + 4 ? parts[at + 2] : undefined;
}

/** Booted-snapshot entries kept per run dir (newest by mtime). */
const PATCHED_KEEP = 8;
/** Card dirs kept in the card cache (newest by mtime). */
const CARDS_KEEP = 32;
/** A card dir touched within this time is never evicted: another worker may have loaded it. */
const CARDS_MIN_AGE_MS = 30 * 60_000;

function newestFirst(dir: string): { full: string; mtime: number }[] {
  return fs
    .readdirSync(dir)
    .filter((name) => !name.startsWith('.'))
    .map((name) => ({ full: path.join(dir, name), mtime: fs.statSync(path.join(dir, name)).mtimeMs }))
    .sort((a, b) => b.mtime - a.mtime);
}

/** Evict card dirs of the card cache beyond the newest CARDS_KEEP (cards are touched on reuse). */
export function evictCards(): void {
  const root = cardCacheRoot();
  if (!fs.existsSync(root)) return;
  for (const entry of newestFirst(root).slice(CARDS_KEEP))
    if (Date.now() - entry.mtime > CARDS_MIN_AGE_MS) fs.rmSync(entry.full, { recursive: true, force: true });
}

/** One run dir per device and vitest worker process at a time, so no two daemons share it. Dirs are reused
 * across processes (a pid lock file marks a dir as claimed), so the booted-snapshot cache stays warm: a cold
 * boot once per dir, then about 1 s. The dir count is bounded by the number of concurrent workers.
 * With a daemon from the shared build store the dirs sit below the daemon's build key
 * (`dist/session-cache/<key12>/<device>-<n>`): snapshots carry no version, so a new daemon build must not
 * load those of an older one. Keeps the newest PATCHED_KEEP snapshot entries. */
export function cachedRunDir(device: Device): string {
  const known = claimedRunDirs.get(device);
  if (known) return known;
  let base = path.join(repoRoot(), 'dist/session-cache');
  const key = daemonKey(daemonBinary(device));
  if (key) base = path.join(base, key.slice(0, 12));
  fs.mkdirSync(base, { recursive: true });
  let dir = '';
  let lock = '';
  for (let i = 0; ; i++) {
    dir = path.join(base, `${device}-${i}`);
    lock = `${dir}.lock`;
    if (tryLock(lock)) break;
  }
  process.on('exit', () => fs.rmSync(lock, { force: true }));
  claimedRunDirs.set(device, dir);
  fs.mkdirSync(dir, { recursive: true });
  const patched = path.join(dir, 'patched');
  if (fs.existsSync(patched))
    for (const entry of newestFirst(patched).slice(PATCHED_KEEP))
      fs.rmSync(entry.full, { recursive: true, force: true });
  return dir;
}

/**
 * The run dir option of a runtime fixture that wants snapshot restores: the worker's persistent dir, emptied except for `patched/`
 * (sockets, logs and per-run files do not persist). Without a build key (a custom `DIGI_EMUD_<DEV>`) it is empty: the session
 * uses a fresh temporary dir, as before. Call it once per daemon start.
 */
export function snapshotRunDir(device: Device): { runDir?: string } {
  if (!daemonKey(daemonBinary(device))) return {};
  const dir = cachedRunDir(device);
  for (const name of fs.readdirSync(dir))
    if (name !== 'patched') fs.rmSync(path.join(dir, name), { recursive: true, force: true });
  evictCards();
  return { runDir: dir };
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
