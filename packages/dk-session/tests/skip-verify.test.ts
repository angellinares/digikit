import { describe, expect, test } from 'vitest';
import { CAP } from '../src/gen_proto.ts';
import type { DkInfoFull } from '../src/dkupload-core.ts';
import { skipVerifyAfterBulk } from '../src/samples.ts';

const info = (caps: number | null) => ({ caps }) as unknown as DkInfoFull;
const withReadback = info(1 << CAP.READBACK);
const without = info(1 << CAP.RENAME);

describe('skipVerifyAfterBulk', () => {
  test('the store skips the SysEx verify after a bulk END only with READBACK', () => {
    expect(skipVerifyAfterBulk('auto', 'bulk', 'digikit', withReadback, false)).toBe(true);
    expect(skipVerifyAfterBulk('auto', 'bulk', 'digikit', without, false)).toBe(false);
    expect(skipVerifyAfterBulk('auto', 'bulk', 'digikit', info(null), false)).toBe(false);
    expect(skipVerifyAfterBulk('auto', 'bulk', 'digikit', null, false)).toBe(false);
  });
  test('always keeps the verify, never skips it, a non-bulk put keeps it', () => {
    expect(skipVerifyAfterBulk('always', 'bulk', 'digikit', withReadback, false)).toBe(false);
    expect(skipVerifyAfterBulk('never', 'sysex', 'digikit', without, false)).toBe(true);
    expect(skipVerifyAfterBulk('auto', 'sysex', 'digikit', withReadback, false)).toBe(false);
  });
  test('the Digitakt II +Drive skips on bulk and keeps the verify when not identified', () => {
    expect(skipVerifyAfterBulk('auto', 'bulk', 'stock-rpc', null, true)).toBe(true);
    expect(skipVerifyAfterBulk('auto', 'bulk', 'stock-rpc', null, false)).toBe(false);
    expect(skipVerifyAfterBulk('auto', 'sysex', 'stock-rpc', null, true)).toBe(false);
  });
});
