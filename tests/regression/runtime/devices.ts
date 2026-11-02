import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, type Device } from '@digi/test-support';

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

/** One run dir per device and vitest worker process at a time, so no two daemons share it. Dirs are reused
 * across processes (a pid lock file marks a dir as claimed), so the booted-snapshot cache stays warm: a cold
 * boot once per dir, then about 1 s. The dir count is bounded by the number of concurrent workers.
 * Keeps the newest 4 snapshot entries. */
export function cachedRunDir(device: Device): string {
  const known = claimedRunDirs.get(device);
  if (known) return known;
  const base = path.join(repoRoot(), 'dist/session-cache');
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
  if (fs.existsSync(patched)) {
    const entries = fs
      .readdirSync(patched)
      .map((name) => ({ full: path.join(patched, name), mtime: fs.statSync(path.join(patched, name)).mtimeMs }))
      .sort((a, b) => b.mtime - a.mtime);
    for (const entry of entries.slice(4)) fs.rmSync(entry.full, { recursive: true, force: true });
  }
  return dir;
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
