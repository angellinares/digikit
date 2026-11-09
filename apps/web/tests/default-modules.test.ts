// Default modules (apps/web/src/firmware/default-modules-core.ts): the unticked choices and the install run over a fake device.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import {
  DefaultModuleError,
  installDefaults,
  parseUnticked,
  patchSetFor,
  tickedIds,
  withTick,
  type DefaultOps,
} from '../src/firmware/default-modules-core.ts';

const entries = [
  { id: 'digikit/remote', summary: 'Remote control over USB' },
  { id: 'digikit/debug', summary: 'Debug' },
];

test('entries are ticked unless the user unticked them, per device', () => {
  assert.deepEqual(tickedIds(entries, {}, 'dt2'), ['digikit/remote', 'digikit/debug']);
  let u = withTick({}, 'dt2', 'digikit/debug', false);
  assert.deepEqual(tickedIds(entries, u, 'dt2'), ['digikit/remote']);
  assert.deepEqual(tickedIds(entries, u, 'dn2'), ['digikit/remote', 'digikit/debug']);
  u = withTick(u, 'dt2', 'digikit/debug', true);
  assert.deepEqual(tickedIds(entries, u, 'dt2'), ['digikit/remote', 'digikit/debug']);
});

test('the saved choice is read back, and garbage is no choice', () => {
  const u = withTick({}, 'dn2', 'digikit/remote', false);
  assert.deepEqual(parseUnticked(JSON.stringify(u)), u);
  for (const bad of [null, '', 'x', '[]', '{"dt2":"a"}', '{"dt2":[1]}']) assert.deepEqual(parseUnticked(bad), {});
});

class Fake implements DefaultOps {
  paths = new Map([
    ['digikit/remote', 'modules/remote.dkm'],
    ['digikit/debug', 'modules/debug.dkm'],
    ['fx/other', 'modules/other.dkm'],
  ]);
  card = new Set<string>();
  now: string[] = [];
  selected: string[][] = [];
  calls: string[] = [];
  failUpload: string | null = null;
  loadsAfterRestart = true;
  pathOf(id: string) {
    return this.paths.get(id);
  }
  async waitForRuntime() {
    this.calls.push('wait');
  }
  async upload(id: string) {
    this.calls.push(`upload ${id}`);
    if (this.failUpload === id) throw new Error('E_NO_SPACE');
    const path = this.paths.get(id)!;
    if (this.card.has(path)) return 'same' as const;
    this.card.add(path);
    return 'uploaded' as const;
  }
  async loaded() {
    return this.now;
  }
  async select(paths: string[]) {
    this.calls.push('select');
    this.selected.push(paths);
  }
  async restart() {
    this.calls.push('restart');
    if (this.loadsAfterRestart)
      this.now = [
        ...new Set([...this.now, ...this.selected.at(-1)!.map((p) => [...this.paths].find(([, v]) => v === p)![0])]),
      ];
    return this.now;
  }
}

test('uploads each module, selects the ones not loaded (the loaded stay), restarts once', async () => {
  const d = new Fake();
  d.now = ['fx/other'];
  const seen: string[] = [];
  const lines = await installDefaults(d, ['digikit/remote', 'digikit/debug'], (p) =>
    seen.push(`${p.done}/${p.total} ${p.text}`),
  );
  assert.deepEqual(d.calls, ['wait', 'upload digikit/remote', 'upload digikit/debug', 'select', 'restart']);
  assert.deepEqual(d.selected, [['modules/other.dkm', 'modules/remote.dkm', 'modules/debug.dkm']]);
  assert.deepEqual(lines, [
    'digikit/remote: uploaded',
    'digikit/debug: uploaded',
    'enabled digikit/remote, digikit/debug (restarted once)',
  ]);
  assert.match(seen[1]!, /^1\/4 Installing digikit\/remote \(1 of 2\)$/);
  assert.match(seen.at(-1)!, /^4\/4 /);
});

test('a module that is on the card and loaded costs no select and no restart', async () => {
  const d = new Fake();
  d.card.add('modules/remote.dkm');
  d.now = ['digikit/remote'];
  const lines = await installDefaults(d, ['digikit/remote']);
  assert.deepEqual(d.calls, ['wait', 'upload digikit/remote']);
  assert.deepEqual(lines, ['digikit/remote: already on the card', 'already enabled']);
});

test('no ticked module: nothing is asked of the device', async () => {
  const d = new Fake();
  assert.deepEqual(await installDefaults(d, []), []);
  assert.deepEqual(d.calls, []);
});

test('a failed upload names the module and stops before the selection', async () => {
  const d = new Fake();
  d.failUpload = 'digikit/debug';
  await assert.rejects(installDefaults(d, ['digikit/remote', 'digikit/debug']), (e: unknown) => {
    assert.ok(e instanceof DefaultModuleError);
    assert.equal(e.id, 'digikit/debug');
    assert.equal(e.message, 'digikit/debug: E_NO_SPACE');
    return true;
  });
  assert.ok(!d.calls.includes('select'));
});

test('a module without a built .dkm fails before anything is sent', async () => {
  const d = new Fake();
  d.paths.delete('digikit/debug');
  await assert.rejects(installDefaults(d, ['digikit/remote', 'digikit/debug']), /digikit\/debug: no built \.dkm/);
  assert.deepEqual(d.calls, []);
});

test('a module that does not load after the restart is reported', async () => {
  const d = new Fake();
  d.loadsAfterRestart = false;
  await assert.rejects(installDefaults(d, ['digikit/remote']), /digikit\/remote: it is not loaded after the restart/);
});

test('a selection over 8 paths is refused without a change on the device', async () => {
  const d = new Fake();
  d.now = Array.from({ length: 8 }, (_, i) => `x/${i}`);
  for (const id of d.now) d.paths.set(id, `modules/${id.slice(2)}.dkm`);
  await assert.rejects(installDefaults(d, ['digikit/remote']), /the limit is 8/);
  assert.ok(!d.calls.includes('select'));
});

test('the Custom boot screen toggle picks the patch set that adds core/introblank', () => {
  assert.equal(patchSetFor(false), 'device');
  assert.equal(patchSetFor(true), 'device-introblank');
});
