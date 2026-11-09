// The data side of the desktop's default modules: the list comes from the device files and the modules' own module.toml; the wait for the runtime.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkError } from '@digi/dk-session';
import { shouldInstallAtStart } from '../src/default-modules.ts';
import {
  defaultModuleIds,
  entries,
  summaryOf,
  waitForRuntime,
} from '../../../web/src/firmware/default-modules-data.ts';

test('both device files list digikit/debug and digikit/remote, and each entry has the summary of its module.toml', () => {
  for (const device of ['dt2', 'dn2']) {
    const list = entries(device);
    assert.deepEqual(
      list.map((e) => e.id),
      ['digikit/debug', 'digikit/remote'],
    );
    assert.match(list[0]!.summary, /log/);
    assert.match(list[1]!.summary, /USB MIDI/);
  }
  assert.deepEqual(entries('st1'), []);
});

test('a device file without the key has no default modules, and a wrong value is an error', () => {
  assert.deepEqual(defaultModuleIds('[digikit]\nid = 1\n'), []);
  assert.deepEqual(defaultModuleIds('[digikit]\ndefault_modules = ["a/b", "c/d"]\n'), ['a/b', 'c/d']);
  assert.throws(() => defaultModuleIds('[digikit]\ndefault_modules = "a/b"\n'), /not a list of module ids/);
  assert.throws(() => defaultModuleIds('[digikit]\ndefault_modules = [1]\n'), /not a list of module ids/);
});

test('summaryOf finds a module by id and says nothing for an unknown one', () => {
  const texts = ['[module]\nid = "x/one"\nsummary = "One."\n', '[module]\nid = "x/two"\nsummary = "Two."\n'];
  assert.equal(summaryOf(texts, 'x/two'), 'Two.');
  assert.equal(summaryOf(texts, 'x/none'), '');
});

test('waitForRuntime asks again until a runtime answers, and gives up with the last reason', async () => {
  let t = 0;
  const now = () => t;
  const sleep = async (ms: number) => void (t += ms);
  const answers: (() => { runtime: number })[] = [
    () => {
      throw new Error('no reply from the device');
    },
    () => ({ runtime: 0 }),
    () => ({ runtime: 5 }),
  ];
  let i = 0;
  await waitForRuntime({ info: async () => answers[i++]!() as never }, { ms: 10_000, everyMs: 1000 }, sleep, now);
  assert.equal(i, 3);
  assert.equal(t, 2000);
  await assert.rejects(
    waitForRuntime(
      {
        info: async () => {
          throw new Error('no reply from the device');
        },
      },
      { ms: 3000, everyMs: 1000 },
      sleep,
      now,
    ),
    /does not answer INFO within 3 s \(no reply from the device\)/,
  );
  await assert.rejects(
    waitForRuntime(
      {
        info: async () => {
          throw new DkError({ error: 'cancelled', kind: 'cancelled' });
        },
      },
      { ms: 3000, everyMs: 1000 },
      sleep,
      now,
    ),
    DkError,
  );
});

test('the install starts at launch only with the runtime on and set, a status, and no install yet', () => {
  const base = { runtimeOn: true, runtimeSet: true, started: false, hasStatus: true };
  assert.equal(shouldInstallAtStart(base), true);
  assert.equal(shouldInstallAtStart({ ...base, runtimeOn: false }), false);
  assert.equal(shouldInstallAtStart({ ...base, runtimeSet: false }), false);
  assert.equal(shouldInstallAtStart({ ...base, started: true }), false);
  assert.equal(shouldInstallAtStart({ ...base, hasStatus: false }), false);
});
