// The store path of a module's own file (moduleFilePath in src/modules/dktransfer.ts): the rule of dk_data_path in sdk/include/dk/platform/data.h.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { moduleFilePath } from '../src/modules/dktransfer.ts';

test('a file name goes under data/<module id>/', () => {
  assert.equal(moduleFilePath('tools/bootscreen', 'intro.dkbs'), 'data/tools/bootscreen/intro.dkbs');
});

test('an empty name, a slash or ".." is refused', () => {
  for (const name of ['', 'a/b', '/a', '..', 'a..b', '../x'])
    assert.throws(() => moduleFilePath('tools/bootscreen', name), name);
});

test('a name of 48 characters or more is refused', () => {
  assert.equal(moduleFilePath('tools/bootscreen', 'a'.repeat(47)), `data/tools/bootscreen/${'a'.repeat(47)}`);
  assert.throws(() => moduleFilePath('tools/bootscreen', 'a'.repeat(48)), /shorter than 48/);
});
