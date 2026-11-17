import { expect, it } from 'vitest';
import { checkAcceptanceTags } from '../acceptance-tags.mjs';

const modules = new Map([
  ['core/fx', { category: 'core', root: 'modules/core/adapters/fx' }],
  ['digikit/debug', { category: 'tools', root: 'modules/tools/debug' }],
  ['example/machine', { category: 'machine', root: 'modules/machines/example' }],
]);
const rec = (files: string[], tags?: unknown) => ({ files, ...(tags === undefined ? {} : { tags }) });

it('accepts tagged records that cover every device test', () => {
  const recs = [
    rec(['a.device.test.ts'], ['S:lfo', 'A:core/fx']),
    rec(['b.device.test.ts'], ['M:digikit/debug', 'C:layout']),
  ];
  expect(() => checkAcceptanceTags('p', recs, ['a.device.test.ts', 'b.device.test.ts'], modules)).not.toThrow();
});

it('throws for a device test in no record, or in a record with no tags', () => {
  expect(() =>
    checkAcceptanceTags(
      'p',
      [rec(['a.device.test.ts'], ['C:boot'])],
      ['a.device.test.ts', 'b.device.test.ts'],
      modules,
    ),
  ).toThrow(/no tags for the device tests: b\.device\.test\.ts/);
  expect(() =>
    checkAcceptanceTags(
      'p',
      [rec(['a.device.test.ts']), rec(['b.device.test.ts'], [])],
      ['a.device.test.ts', 'b.device.test.ts'],
      modules,
    ),
  ).toThrow(/a\.device\.test\.ts, b\.device\.test\.ts/);
});

it('throws for a tag outside the vocabulary', () => {
  const check = (tag: unknown) => () =>
    checkAcceptanceTags('p', [rec(['a.device.test.ts'], [tag])], ['a.device.test.ts'], modules);
  expect(check('S:bass')).toThrow(/names no service/);
  expect(check('C:speed')).toThrow(/names no area/);
  expect(check('A:core/nothing')).toThrow(/names no module/);
  expect(check('A:example/machine')).toThrow(/not of category core/);
  expect(check('M:core/fx')).toThrow(/not under modules\/tools/);
  expect(check('fx')).toThrow(/no S:, C:, A: or M: prefix/);
  expect(check(3)).toThrow(/not a string/);
});

it('checks the tags of a record without a device test list', () => {
  expect(() => checkAcceptanceTags('p', [rec(['e.test.ts'], ['C:bogus'])], [], modules)).toThrow(/names no area/);
});
