import fs from 'node:fs';
import path from 'node:path';

/**
 * Persistent run dirs of the device fixtures. A dir is claimed with a pid lock file (`<dir>.lock`) while a daemon runs in it. What the
 * daemon cached for a card (`cards/`) stays for the next claim; everything else is per run and is removed at the claim.
 * The booted-state snapshots are not in the run dir: all run dirs of a daemon build and device share the store `<key>/snapshots/<device>`
 * (`DIGI_SNAPSHOT_DIR`). Its entries are named by their content key, so any daemon of the build may use any entry.
 */

/** Names in a run dir that survive from one claim to the next. */
const KEEP = new Set(['cards']);
/** Size of the shared snapshot store of one daemon build and device; the oldest entries (by mtime) go first at a claim. */
export const SNAPSHOTS_MAX_BYTES = 6 * 1024 ** 3;
/** Failed run dirs kept per device. */
export const FAILED_KEEP = 2;
/** Daemon-key dirs kept under the session cache (the newest by mtime). */
export const KEY_DIRS_KEEP = 2;
/** Marker of a run dir in use; only a clean release removes it. */
const DIRTY = '.dirty';
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

const lockText = (lock: string): string | undefined => {
  try {
    return fs.readFileSync(lock, 'utf8');
  } catch {
    return undefined;
  }
};

let staleCount = 0;

let failedSeq = 0;
/** The name a failed run dir gets. The time alone can repeat within a millisecond. */
const failedName = (dir: string): string => `${dir}.failed-${Date.now()}-${process.pid}-${failedSeq++}`;

/**
 * Take the lock for this process. The file appears with its pid in it: it is written aside and linked into place, which fails if the lock
 * exists. A stale lock is renamed away first, then the link is tried again: only the process whose link succeeds owns the lock. When the
 * file that a process renamed away is not the stale one (another process took the lock meanwhile), it is put back.
 */
export function tryLock(lock: string): boolean {
  const tmp = `${lock}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, String(process.pid));
  try {
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        fs.linkSync(tmp, lock);
        return true;
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      }
      if (held(lock)) return false;
      const seen = lockText(lock);
      const away = `${lock}.stale-${process.pid}-${++staleCount}`;
      try {
        fs.renameSync(lock, away);
      } catch {
        continue; // another process removed it first: try the link again
      }
      const moved = lockText(away);
      if (moved !== seen) {
        try {
          fs.linkSync(away, lock); // a live lock that we took by mistake: give it back
        } catch {
          /* the lock is taken again; the owner is not harmed */
        }
        fs.rmSync(away, { force: true });
        return false;
      }
      fs.rmSync(away, { force: true });
    }
    return false;
  } finally {
    fs.rmSync(tmp, { force: true });
  }
}

/** The names in DIR; none if it is gone (a concurrent pruner may have removed it). */
function readNames(dir: string): string[] {
  try {
    return fs.readdirSync(dir);
  } catch {
    return [];
  }
}

export function newestFirst(dir: string): { full: string; mtime: number }[] {
  return readNames(dir)
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

/** Delete the oldest entries (files or dirs; a hit touches its entry) of the snapshot store until it is at most SNAPSHOTS_MAX_BYTES. */
export function pruneSnapshots(store: string, maxBytes = SNAPSHOTS_MAX_BYTES): void {
  const size = (full: string): number => {
    try {
      const stat = fs.statSync(full);
      return stat.isDirectory()
        ? readNames(full).reduce((sum, name) => sum + size(path.join(full, name)), 0)
        : stat.size;
    } catch {
      return 0; // removed meanwhile
    }
  };
  const entries = newestFirst(store).map((entry) => ({ ...entry, bytes: size(entry.full) }));
  let total = entries.reduce((sum, entry) => sum + entry.bytes, 0);
  for (const entry of entries.reverse()) {
    if (total <= maxBytes) break;
    fs.rmSync(entry.full, { recursive: true, force: true });
    total -= entry.bytes;
  }
}

/** Delete the failed run dirs (`<device>-<n>.failed-<time>`) of DEVICE beyond the newest FAILED_KEEP. */
export function pruneFailed(base: string, device: string): void {
  const failed = newestFirst(base).filter(
    (e) => path.basename(e.full).startsWith(`${device}-`) && e.full.includes('.failed-'),
  );
  for (const entry of failed.slice(FAILED_KEEP)) fs.rmSync(entry.full, { recursive: true, force: true });
}

/**
 * Delete the daemon-key dirs of SESSION_CACHE (the dirs `<key12>`), except for the newest KEY_DIRS_KEEP of each device (a key dir serves the
 * devices whose `<device>-<n>` dirs it holds) and those where a live process holds a lock.
 */
export function pruneKeyDirs(sessionCache: string): void {
  const keyDirs = newestFirst(sessionCache).filter((e) => /^[0-9a-f]{12}$/.test(path.basename(e.full)));
  const keep = new Set<string>();
  for (const device of new Set(
    keyDirs.flatMap((e) => readNames(e.full).map((n) => /^([a-z0-9]+)-\d+$/.exec(n)?.[1] ?? '')),
  )) {
    if (!device) continue;
    keyDirs
      .filter((e) => readNames(e.full).some((n) => n.startsWith(`${device}-`)))
      .slice(0, KEY_DIRS_KEEP)
      .forEach((e) => keep.add(e.full));
  }
  for (const entry of keyDirs) {
    if (keep.has(entry.full)) continue;
    const busy = readNames(entry.full).some((name) => name.endsWith('.lock') && held(path.join(entry.full, name)));
    if (!busy) fs.rmSync(entry.full, { recursive: true, force: true });
  }
}

export interface RunDirClaim {
  readonly dir: string;
  /** The shared snapshot store of this daemon build and device (a daemon gets it as `DIGI_SNAPSHOT_DIR`). */
  readonly snapshotDir: string;
  /** Give the dir back. A failed run's dir is renamed `<dir>.failed-<time>`: its snapshots are not reused and its files stay for diagnosis. Idempotent. */
  release(failed?: boolean): void;
}

/**
 * Claim the first free `<device>-<n>` dir in BASE. `wipe` removes everything but the daemon's caches. A `.dirty` marker is written at the
 * claim and removed by a clean release only: a dir that still has it (the process was killed, timed out or exited with the file unfinished)
 * is renamed like a failed one, so its snapshots are not reused.
 */
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
  const dirty = path.join(dir, DIRTY);
  if (fs.existsSync(dirty)) fs.renameSync(dir, failedName(dir));
  fs.mkdirSync(dir, { recursive: true });
  if (wipe)
    for (const name of readNames(dir))
      if (!KEEP.has(name)) fs.rmSync(path.join(dir, name), { recursive: true, force: true });
  const snapshotDir = path.join(base, 'snapshots', device);
  fs.mkdirSync(snapshotDir, { recursive: true });
  pruneSnapshots(snapshotDir);
  pruneFailed(base, device);
  fs.writeFileSync(dirty, String(process.pid));
  const unlock = () => {
    if (lockText(lock) === String(process.pid)) fs.rmSync(lock, { force: true });
  };
  let released = false;
  const claim: RunDirClaim = {
    dir,
    snapshotDir,
    release(failed = false) {
      if (released) return;
      released = true;
      try {
        if (failed) fs.renameSync(dir, failedName(dir));
        else fs.rmSync(dirty, { force: true });
      } finally {
        unlock();
      }
    },
  };
  // A process that exits with the claim still held leaves the marker: the dir is not trusted.
  process.on('exit', () => {
    if (!released) unlock();
  });
  return claim;
}
