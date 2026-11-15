import { createHash } from 'node:crypto';
import { expect, test } from 'vitest';
import {
  edits,
  entry,
  H,
  nameHash,
  patched,
  TAG,
  unhashed,
  usesCountAt,
} from '../../../modules/core/runtime/tests/refusal/dkm-patch.ts';

/** A file with the header fields the edits touch and a header table of a NEEDS, a USES and a KEYS entry (the layout of packages/dkneeds/src/keys.rs). */
function fixture(): Buffer {
  const f = Buffer.alloc(0x100);
  f.writeUInt32BE(0x444b4d46, H.MAGIC);
  f.writeUInt32BE(0x000802, H.RUNTIME);
  f.write('test/fixture', H.NAME);
  const table = 0xa0;
  f.writeUInt32BE(table, H.KEYS_OFF);
  f.writeUInt32BE(0x444b5442, table); // DKTB
  f.writeUInt16BE(1, table + 4);
  f.writeUInt16BE(3, table + 6);
  let at = table + 8;
  // NEEDS: one item (hash, version, flags)
  f.writeUInt16BE(1, at);
  f.writeUInt16BE(1, at + 2);
  f.writeUInt32BE(12, at + 4);
  f.writeUInt32BE(nameHash('ui'), at + 8);
  f.writeUInt32BE(0x000700, at + 12);
  f.writeUInt32BE(1, at + 16);
  at += 8 + 12;
  // USES: machine rows 1, param rows 3
  f.writeUInt16BE(2, at);
  f.writeUInt16BE(2, at + 2);
  f.writeUInt32BE(16, at + 4);
  f.writeUInt32BE(1, at + 8);
  f.writeUInt32BE(1, at + 12);
  f.writeUInt32BE(2, at + 16);
  f.writeUInt32BE(3, at + 20);
  at += 8 + 16;
  // KEYS: one group, kind 1, one key
  f.writeUInt16BE(3, at);
  f.writeUInt16BE(1, at + 2);
  f.writeUInt32BE(12, at + 4);
  f.writeUInt32BE(1, at + 8);
  f.writeUInt32BE(1, at + 12);
  f.writeUInt32BE(0xa6513f00, at + 16);
  f.writeUInt32BE(table + 8 + 12 + 8 + 16 + 8 + 12 - table, H.KEYS_SIZE);
  return f;
}

/** The loader's check of the file's own hash: sha256 over the file with the sha field read as zeros. */
function shaOk(f: Buffer): boolean {
  const copy = Buffer.from(f);
  copy.fill(0, H.SHA, H.SHA + 32);
  return createHash('sha256')
    .update(copy)
    .digest()
    .equals(f.subarray(H.SHA, H.SHA + 32));
}

test('patched keeps the file consistent: the name changes, the hash is right again, unhashed breaks only the hash', () => {
  const base = fixture();
  const f = patched(base, 'test/copy');
  expect(f.subarray(H.NAME, H.NAME + 9).toString()).toBe('test/copy');
  expect(f[H.NAME + 9], 'the old name does not show behind the new one').toBe(0);
  expect(shaOk(f)).toBe(true);
  expect(base.subarray(H.NAME, H.NAME + 12).toString(), 'the fixture itself is not changed').toBe('test/fixture');
  expect(shaOk(unhashed(f))).toBe(false);
});

test('each edit changes exactly the field of its stage', () => {
  const base = fixture();
  const diff = (e: (f: Buffer) => void) => {
    const f = patched(base, 'test/fixture', e);
    const at: number[] = [];
    for (let i = 0; i < f.length; i += 4)
      if (i < H.SHA || i >= H.SHA + 32) if (!f.subarray(i, i + 4).equals(base.subarray(i, i + 4))) at.push(i);
    return at;
  };
  expect(diff(edits.badMagic)).toEqual([H.MAGIC]);
  expect(diff(edits.newerRuntime)).toEqual([H.RUNTIME]);
  const need = entry(base, TAG.NEEDS).at;
  expect(diff(edits.needsNewerRuntime)).toEqual([need, need + 4]);
  expect(diff(edits.needsMissingService)).toEqual([need, need + 4]);
  expect(diff(edits.unownedKind(90))).toEqual([entry(base, TAG.KEYS).at]);
  expect(diff(edits.machineRows(1, 99))).toEqual([usesCountAt(base, 1)]);
  expect(patched(base, 'x', edits.newerRuntime).readUInt32BE(H.RUNTIME)).toBe(0x010802);
});

test('entry and usesCountAt find the payloads and say so when an entry is missing', () => {
  const f = fixture();
  expect(entry(f, TAG.USES).count).toBe(2);
  expect(f.readUInt32BE(usesCountAt(f, 2))).toBe(3);
  expect(() => usesCountAt(f, 9)).toThrow(/resource 9/);
  expect(() => entry(f, 7)).toThrow(/tag 7/);
});

test('nameHash is the FNV-1a of dkneeds', () => {
  expect(nameHash('')).toBe(0x811c9dc5);
  expect(nameHash('a')).toBe(0xe40c292c);
});
