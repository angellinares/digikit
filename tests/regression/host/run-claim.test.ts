import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, expect, test } from 'vitest';
import { claimDir, FAILED_KEEP, pruneKeyDirs, tryLock } from '../runtime/run-claim.ts';

let base: string;
beforeEach(() => {
  base = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-claim-'));
});
afterEach(() => fs.rmSync(base, { recursive: true, force: true }));

const old = (file: string, ms: number) => fs.utimesSync(file, new Date(Date.now() - ms), new Date(Date.now() - ms));

test('a lock is taken once, with the pid in it from the start', () => {
  const lock = path.join(base, 'a.lock');
  expect(tryLock(lock)).toBe(true);
  expect(fs.readFileSync(lock, 'utf8')).toBe(String(process.pid));
  expect(tryLock(lock)).toBe(false);
  expect(fs.readdirSync(base)).toEqual(['a.lock']);
});

test('the lock of a dead process is taken over', () => {
  const lock = path.join(base, 'a.lock');
  fs.writeFileSync(lock, '2147483646');
  expect(tryLock(lock)).toBe(true);
});

test('an empty or unreadable lock is held until it is 10 minutes old', () => {
  for (const text of ['', 'junk']) {
    const lock = path.join(base, `${text || 'empty'}.lock`);
    fs.writeFileSync(lock, text);
    expect(tryLock(lock)).toBe(false);
    old(lock, 11 * 60_000);
    expect(tryLock(lock)).toBe(true);
  }
});

test('a claim wipes per-run files but keeps the daemon caches, and a release frees the dir', () => {
  const first = claimDir(base, 'dt2', true);
  fs.mkdirSync(path.join(first.dir, 'patched'));
  fs.writeFileSync(path.join(first.dir, 'patched', 'snap'), 'x');
  fs.mkdirSync(path.join(first.dir, 'boot-1'));
  fs.writeFileSync(path.join(first.dir, 'daemon.stderr'), 'x');
  const second = claimDir(base, 'dt2', true);
  expect(second.dir).not.toBe(first.dir);
  first.release();
  first.release();
  const again = claimDir(base, 'dt2', true);
  expect(again.dir).toBe(first.dir);
  expect(fs.readdirSync(again.dir).sort()).toEqual(['.dirty', 'patched']);
  expect(fs.existsSync(path.join(again.dir, 'patched', 'snap'))).toBe(true);
});

test('a failed run dir is renamed, its snapshots are not reused, and only the newest ones stay', () => {
  for (let i = 0; i < FAILED_KEEP + 2; i++) {
    const claim = claimDir(base, 'dn2', true);
    fs.mkdirSync(path.join(claim.dir, 'patched'));
    fs.writeFileSync(path.join(claim.dir, 'patched', 'snap'), String(i));
    claim.release(true);
    expect(fs.existsSync(claim.dir)).toBe(false);
    // `.failed-<ms>` names differ only by time
    for (const name of fs.readdirSync(base).filter((n) => n.includes('.failed-')))
      old(path.join(base, name), (FAILED_KEEP + 2 - i) * 1000);
  }
  const next = claimDir(base, 'dn2', true);
  expect(fs.existsSync(path.join(next.dir, 'patched'))).toBe(false);
  expect(fs.readdirSync(base).filter((n) => n.includes('.failed-'))).toHaveLength(FAILED_KEEP);
});

test('a dir that was not released cleanly is not reused; a clean release removes the marker', () => {
  const clean = claimDir(base, 'dt2', true);
  fs.mkdirSync(path.join(clean.dir, 'patched'));
  expect(fs.existsSync(path.join(clean.dir, '.dirty'))).toBe(true);
  clean.release();
  expect(fs.existsSync(path.join(clean.dir, '.dirty'))).toBe(false);
  const reused = claimDir(base, 'dt2', true);
  expect(reused.dir).toBe(clean.dir);
  expect(fs.existsSync(path.join(reused.dir, 'patched'))).toBe(true);
  // the process dies: the lock is stale and the marker stays
  fs.writeFileSync(`${reused.dir}.lock`, '2147483646');
  const next = claimDir(base, 'dt2', true);
  expect(next.dir).toBe(reused.dir);
  expect(fs.existsSync(path.join(next.dir, 'patched'))).toBe(false);
  expect(fs.readdirSync(base).filter((n) => n.includes('.failed-'))).toHaveLength(1);
});

test('a lock taken over from a dead process is not taken from its new owner, and release keeps a lock that is not ours', () => {
  const lock = path.join(base, 'a.lock');
  fs.writeFileSync(lock, '2147483646');
  expect(tryLock(lock)).toBe(true);
  expect(tryLock(lock)).toBe(false);
  expect(fs.readdirSync(base)).toEqual(['a.lock']);
  const claim = claimDir(base, 'dn2', true);
  fs.writeFileSync(`${claim.dir}.lock`, String(process.ppid)); // another live process owns it now
  claim.release();
  expect(fs.readFileSync(`${claim.dir}.lock`, 'utf8')).toBe(String(process.ppid));
});

test('old daemon-key dirs go, except the newest 2 of each device and those with a live lock', () => {
  const keys = ['a', 'b', 'c', 'd', 'e', 'f'].map((c) => c.repeat(12));
  // a, b, c serve dt2 (c the oldest); d, e, f serve dn2 (f the oldest); b also holds a live lock
  keys.forEach((key, i) => {
    fs.mkdirSync(path.join(base, key, `${i < 3 ? 'dt2' : 'dn2'}-0`), { recursive: true });
    old(path.join(base, key), (i + 1) * 60_000);
  });
  old(path.join(base, keys[2]!), 10 * 60_000);
  old(path.join(base, keys[5]!), 11 * 60_000);
  pruneKeyDirs(base);
  expect(fs.readdirSync(base).sort()).toEqual([keys[0]!, keys[1]!, keys[3]!, keys[4]!].sort());
  fs.writeFileSync(path.join(base, keys[1]!, 'dt2-0.lock'), String(process.pid));
  expect(() => pruneKeyDirs(path.join(base, 'missing'))).not.toThrow();
});
