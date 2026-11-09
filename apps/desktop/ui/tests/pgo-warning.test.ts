// The status bar warning for a build without a valid PGO profile (apps/desktop/ui/src/pgo-warning.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { pgoWarning } from '../src/pgo-warning.ts';

test('no warning when PGO is active or the state is unknown', () => {
  assert.equal(pgoWarning(undefined), undefined);
  assert.equal(pgoWarning({ pgo: null }), undefined);
  assert.equal(pgoWarning({}), undefined);
  assert.equal(pgoWarning({ pgo: 'ok', pgo_detail: '' }), undefined);
  assert.equal(pgoWarning({ pgo: 'something-new' }), undefined);
});

test('a stale profile warns with the reason and the fix', () => {
  const w = pgoWarning({ pgo: 'stale', pgo_detail: 'changed: sources' });
  assert.equal(w?.label, 'no PGO');
  assert.deepEqual(w?.title.split('\n'), [
    'Built without a valid PGO profile: DSP runs about 30% slower.',
    'changed: sources.',
    'Fix: mise run perf:pgo:both, then relaunch.',
  ]);
});

test('an empty reason leaves out the middle line', () => {
  const w = pgoWarning({ pgo: 'missing', pgo_detail: '' });
  assert.equal(w?.title.split('\n').length, 2);
});

test('a mismatch uses its own first sentence', () => {
  const w = pgoWarning({ pgo: 'mismatch', pgo_detail: 'profile does not match this build' });
  assert.equal(w?.title.split('\n')[0], 'This PGO profile does not match the build: DSP runs about 30% slower.');
});
