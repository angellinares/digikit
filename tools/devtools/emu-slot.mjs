// Machine-wide emulator slots: a command prefix for device and conformance targets.
//
//   node tools/devtools/emu-slot.mjs -- <command...>
//
// Every Nx launcher run has its own CPU budget, so several lanes at once can start more emulators than
// the machine runs well. This wrapper takes one of N slots shared by all worktrees of the clone
// (`<git common dir>/emu-slots/slot-<i>`), runs the command with inherited stdio, releases the slot and
// exits with the command's status (a signal n becomes 128+n). It waits without a time limit.
// N is DIGI_EMU_SLOTS, else the launcher's task count (tools/nx/nx.mjs).
//
// Lock rules are those of tests/regression/runtime/run-claim.ts: the lock file appears with the pid in it
// (written aside, then linked into place). A dead pid is taken over by renaming the lock away first. An
// unreadable lock counts as held until it is 10 minutes old. Release removes the lock only if it holds our pid.
import { spawn, spawnSync } from 'node:child_process';
import { constants } from 'node:os';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { budget, workCores } from '../nx/nx.mjs';

const UNREADABLE_LOCK_MS = 10 * 60_000;
const POLL_MS = 500;

const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code !== 'ESRCH';
  }
};

const lockText = (lock) => {
  try {
    return fs.readFileSync(lock, 'utf8');
  } catch {
    return undefined;
  }
};

// Whether the lock names a live process, or is too new to judge. A missing lock is not held.
const held = (lock) => {
  const text = lockText(lock);
  if (text === undefined) return false;
  const pid = Number.parseInt(text, 10);
  if (Number.isInteger(pid) && pid > 0) return alive(pid);
  try {
    return Date.now() - fs.statSync(lock).mtimeMs < UNREADABLE_LOCK_MS;
  } catch {
    return false;
  }
};

let staleCount = 0;

export const tryLock = (lock) => {
  const tmp = `${lock}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, String(process.pid));
  try {
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        fs.linkSync(tmp, lock);
        return true;
      } catch (error) {
        if (error.code !== 'EEXIST') throw error;
      }
      if (held(lock)) return false;
      const seen = lockText(lock);
      const away = `${lock}.stale-${process.pid}-${++staleCount}`;
      try {
        fs.renameSync(lock, away);
      } catch {
        continue; // another process removed it first: try the link again
      }
      if (lockText(away) !== seen) {
        // We moved a live lock that was taken meanwhile: give it back.
        try {
          fs.linkSync(away, lock);
        } catch {
          /* taken again; the owner is not harmed */
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
};

export const slotCount = (env = process.env) => {
  const set = env.DIGI_EMU_SLOTS;
  if (set !== undefined && set !== '') {
    const n = Number(set);
    if (!Number.isInteger(n) || n < 1) throw new Error(`DIGI_EMU_SLOTS must be a positive integer, got ${set}`);
    return n;
  }
  return budget(workCores(), env.DIGI_BUDGET).tasks;
};

const slotDir = () => {
  const done = spawnSync('git', ['rev-parse', '--git-common-dir'], { encoding: 'utf8' });
  if (done.status !== 0) throw new Error('emu-slot: not in a git checkout');
  return path.join(path.resolve(done.stdout.trim()), 'emu-slots');
};

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Take a free slot, waiting as long as needed. Returns the lock path.
export const acquire = async (dir, slots) => {
  fs.mkdirSync(dir, { recursive: true });
  let told = false;
  for (;;) {
    for (let i = 0; i < slots; i++) {
      const lock = path.join(dir, `slot-${i}`);
      if (tryLock(lock)) return lock;
    }
    if (!told) {
      told = true;
      let used = 0;
      for (let i = 0; i < slots; i++) if (held(path.join(dir, `slot-${i}`))) used++;
      console.error(`waiting for an emulator slot (${used} in use)`);
    }
    await sleep(POLL_MS);
  }
};

export const release = (lock) => {
  if (lockText(lock) === String(process.pid)) fs.rmSync(lock, { force: true });
};

const main = async () => {
  const argv = process.argv.slice(2);
  const split = argv.indexOf('--');
  const cmd = split < 0 ? [] : argv.slice(split + 1);
  if (!cmd.length) {
    console.error('usage: node tools/devtools/emu-slot.mjs -- <command...>');
    process.exit(2);
  }
  let child;
  let signalled;
  const forward = (signal) => {
    signalled = signal;
    if (child) child.kill(signal);
    else process.exit(128 + constants.signals[signal]); // still waiting: no slot to release
  };
  process.on('SIGTERM', () => forward('SIGTERM'));
  process.on('SIGINT', () => forward('SIGINT'));
  const lock = await acquire(slotDir(), slotCount());
  process.on('exit', () => release(lock));
  if (signalled) process.exit(128 + constants.signals[signalled]);
  child = spawn(cmd[0], cmd.slice(1), { stdio: 'inherit' });
  child.on('error', (error) => {
    console.error(`emu-slot: ${error.message}`);
    release(lock);
    process.exit(127);
  });
  child.on('exit', (code, signal) => {
    release(lock);
    process.exit(code ?? 128 + (constants.signals[signal] ?? 0));
  });
};

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
