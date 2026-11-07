import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, expect, test } from 'vitest';
import { claimDir, FAILED_KEEP, KEY_DIRS_KEEP, pruneKeyDirs, tryLock } from '../runtime/run-claim.ts';

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
  expect(fs.readdirSync(again.dir)).toEqual(['patched']);
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

test('old daemon-key dirs go, except the newest ones and those with a live lock', () => {
  const keys = ['a', 'b', 'c', 'd'].map((c) => c.repeat(12));
  keys.forEach((key, i) => {
    fs.mkdirSync(path.join(base, key));
    old(path.join(base, key), (i + 1) * 60_000);
  });
  fs.writeFileSync(path.join(base, keys[3]!, 'dt2-0.lock'), String(process.pid));
  old(path.join(base, keys[3]!), 4 * 60_000);
  pruneKeyDirs(base);
  expect(fs.readdirSync(base).sort()).toEqual([keys[0]!, keys[1]!, keys[3]!].slice(0, KEY_DIRS_KEEP + 1).sort());
});
