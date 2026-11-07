import fs from 'node:fs';
import path from 'node:path';

/**
 * Persistent run dirs of the device fixtures. A dir is claimed with a pid lock file (`<dir>.lock`) while a daemon runs in it. What the
 * daemon cached for a boot (`patched/`, `cards/`) stays for the next claim; everything else is per run and is removed at the claim.
 */

/** Names in a run dir that survive from one claim to the next. */
const KEEP = new Set(['patched', 'cards']);
/** Booted-snapshot entries kept per run dir (newest by mtime). */
export const PATCHED_KEEP = 8;
/** Failed run dirs kept per device. */
export const FAILED_KEEP = 2;
/** Daemon-key dirs kept under the session cache (the newest by mtime). */
export const KEY_DIRS_KEEP = 2;
/** A lock file with no readable pid is held until it is this old. */
const UNREADABLE_LOCK_MS = 10 * 60_000;

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

/** Whether the lock file names a live process, or is too new to judge (empty or not a pid). */
function held(lock: string): boolean {
  let text: string;
  let mtime: number;
  try {
    text = fs.readFileSync(lock, 'utf8');
    mtime = fs.statSync(lock).mtimeMs;
  } catch {
    return false; // gone
  }
  const pid = Number.parseInt(text, 10);
  if (Number.isInteger(pid) && pid > 0) return alive(pid);
  return Date.now() - mtime < UNREADABLE_LOCK_MS;
}

/** Take the lock for this process. The file appears with its pid in it: it is written aside and linked into place, which fails if the lock exists. */
export function tryLock(lock: string): boolean {
  const tmp = `${lock}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, String(process.pid));
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        fs.linkSync(tmp, lock);
        return true;
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      }
      if (held(lock)) return false;
      fs.rmSync(lock, { force: true });
    }
    return false;
  } finally {
    fs.rmSync(tmp, { force: true });
  }
}

export function newestFirst(dir: string): { full: string; mtime: number }[] {
  return fs
    .readdirSync(dir)
    .filter((name) => !name.startsWith('.'))
    .flatMap((name) => {
      try {
        return [{ full: path.join(dir, name), mtime: fs.statSync(path.join(dir, name)).mtimeMs }];
      } catch {
        return []; // removed meanwhile
      }
    })
    .sort((a, b) => b.mtime - a.mtime);
}

/** Delete the failed run dirs (`<device>-<n>.failed-<time>`) of DEVICE beyond the newest FAILED_KEEP. */
export function pruneFailed(base: string, device: string): void {
  const failed = newestFirst(base).filter(
    (e) => path.basename(e.full).startsWith(`${device}-`) && e.full.includes('.failed-'),
  );
  for (const entry of failed.slice(FAILED_KEEP)) fs.rmSync(entry.full, { recursive: true, force: true });
}

/** Delete the daemon-key dirs of SESSION_CACHE (the dirs `<key12>` with run dirs) beyond the newest KEY_DIRS_KEEP, unless a live process holds a lock in them. */
export function pruneKeyDirs(sessionCache: string): void {
  const keyDirs = newestFirst(sessionCache).filter((e) => /^[0-9a-f]{12}$/.test(path.basename(e.full)));
  for (const entry of keyDirs.slice(KEY_DIRS_KEEP)) {
    const busy = fs.readdirSync(entry.full).some((name) => name.endsWith('.lock') && held(path.join(entry.full, name)));
    if (!busy) fs.rmSync(entry.full, { recursive: true, force: true });
  }
}

export interface RunDirClaim {
  readonly dir: string;
  /** Give the dir back. A failed run's dir is renamed `<dir>.failed-<time>`: its snapshots are not reused and its files stay for diagnosis. Idempotent. */
  release(failed?: boolean): void;
}

/** Claim the first free `<device>-<n>` dir in BASE. `wipe` removes everything but the daemon's caches. */
export function claimDir(base: string, device: string, wipe: boolean): RunDirClaim {
  fs.mkdirSync(base, { recursive: true });
  const now = new Date();
  fs.utimesSync(base, now, now);
  let dir = '';
  let lock = '';
  for (let i = 0; ; i++) {
    dir = path.join(base, `${device}-${i}`);
    lock = `${dir}.lock`;
    if (tryLock(lock)) break;
  }
  fs.mkdirSync(dir, { recursive: true });
  if (wipe)
    for (const name of fs.readdirSync(dir))
      if (!KEEP.has(name)) fs.rmSync(path.join(dir, name), { recursive: true, force: true });
  const patched = path.join(dir, 'patched');
  if (fs.existsSync(patched))
    for (const entry of newestFirst(patched).slice(PATCHED_KEEP))
      fs.rmSync(entry.full, { recursive: true, force: true });
  pruneFailed(base, device);
  let released = false;
  const claim: RunDirClaim = {
    dir,
    release(failed = false) {
      if (released) return;
      released = true;
      try {
        if (failed) fs.renameSync(dir, `${dir}.failed-${Date.now()}`);
      } finally {
        fs.rmSync(lock, { force: true });
      }
    },
  };
  process.on('exit', () => claim.release());
  return claim;
}
