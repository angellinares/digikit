import fs from 'node:fs';
import path from 'node:path';
import { repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import {
  decodeSettings,
  encodeSettings,
  SETTINGS_AREA,
  SETTINGS_KEY_MAX,
  SETTINGS_VALUE_MAX,
  settingsWhy,
} from '../runtime/project-kit.ts';

interface Vectors {
  area_bytes: number;
  key_max: number;
  value_max: number;
  valid: { name: string; hex: string; seq: number; used: number; entries: [string, string][] }[];
  invalid: { name: string; why: string; hex: string }[];
}

const vectors: Vectors = JSON.parse(
  fs.readFileSync(path.join(repoRoot(), 'sdk/vectors/settings-vectors.json'), 'utf8'),
);

describe('settings record against the shared vectors', () => {
  test('the limits are the vectors limits', () => {
    expect([SETTINGS_AREA, SETTINGS_KEY_MAX, SETTINGS_VALUE_MAX]).toEqual([
      vectors.area_bytes,
      vectors.key_max,
      vectors.value_max,
    ]);
  });

  test.each(vectors.valid)('valid $name: decodes, and the encoder gives the same bytes', (v) => {
    const bytes = Buffer.from(v.hex, 'hex');
    const rec = decodeSettings(bytes);
    expect(rec?.seq).toBe(v.seq);
    expect(rec?.used).toBe(v.used);
    expect(rec?.entries.map(([k, value]) => [k, value.toString('hex')])).toEqual(v.entries);
    // bytes after `used` are ignored by the readers; the encoder writes the record only
    expect(
      encodeSettings(
        v.seq,
        v.entries.map(([k, value]) => [k, Buffer.from(value, 'hex')]),
      ).equals(bytes.subarray(0, v.used)),
    ).toBe(true);
  });

  test.each(vectors.invalid)('invalid $name: refused for the first broken rule ($why)', (v) => {
    const bytes = Buffer.from(v.hex, 'hex');
    expect(decodeSettings(bytes)).toBeNull();
    expect(settingsWhy(bytes)).toBe(v.why);
  });
});
