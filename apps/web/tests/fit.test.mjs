// The fit rule through patchkit.wasm (`pk_fit`, packages/patchkit/src/fit.rs): a round trip over a few shared vectors of sdk/vectors/fit-vectors.json.
// The rule itself is tested once, in Rust (modules/core/runtime/fit). Skips when patchkit.wasm is not built.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';
import { DkNeeds } from '../src/modules/dkneeds.ts';
import { WASM } from './fit-wasm.ts';

const V = JSON.parse(readFileSync(new URL('../../../sdk/vectors/fit-vectors.json', import.meta.url), 'utf8'));
const capsOf = (over) =>
  Object.fromEntries(
    Object.entries({ ...V.caps_base, ...over })
      .filter(([, v]) => v !== null)
      .map(([k, [limit, used, largest]]) => [k, { limit, used, largest }]),
  );
const skip = !existsSync(WASM);

test('each need is judged against the capacity, with the texts of the vectors', { skip }, async () => {
  const reader = await DkNeeds.create(readFileSync(WASM));
  for (const name of [
    'one byte over the largest free range',
    'a file larger than the staging buffer',
    'machine rows',
  ]) {
    const c = V.cases.find((x) => x.name === name);
    const got = reader.fit({ caps: capsOf(c.caps), needs: [c.needs] }).each[0];
    assert.deepEqual([got.resource, got.need, got.free], c.want, name);
  }
  const fits = V.cases.find((x) => x.name === 'a small ColdFire module');
  assert.equal(reader.fit({ caps: capsOf(), needs: [fits.needs] }).each[0], null);
});

test('a set loads in turn and `empty` starts from the limits', { skip }, async () => {
  const reader = await DkNeeds.create(readFileSync(WASM));
  const s = V.sets.find((x) => x.name.startsWith('two pluck-like'));
  const got = reader.fit({ caps: capsOf(s.caps), needs: s.needs }).set;
  assert.deepEqual([got.index, got.resource, got.need, got.free], s.want);
  const used = { ...capsOf(), MODULES: { limit: 8, used: 8, largest: 0 } };
  const n = [{ ARENA: 48 }];
  assert.equal(reader.fit({ caps: used, needs: n }).set.resource, 'MODULES');
  assert.equal(reader.fit({ caps: used, needs: n, empty: true }).set, null);
});

test('a short carries the wordings for a person, and a bad request is refused', { skip }, async () => {
  const reader = await DkNeeds.create(readFileSync(WASM));
  const c = V.cases.find((x) => x.name === 'parameter rows');
  const short = reader.fit({ caps: capsOf(c.caps), needs: [c.needs] }).each[0];
  assert.deepEqual(
    [short.text, short.brief, short.describe, short.describe_by],
    ['PARAMS 3 > 2 FREE', 'PARAMS 3>2', 'parameter rows 3 needed, 2 free', 'parameter rows 1 short'],
  );
  assert.throws(() => reader.fit({ caps: 1, needs: [] }), /caps/);
});
