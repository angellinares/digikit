// The Custom boot screen toggle of the Firmware page (apps/web/src/firmware/default-modules-state.ts): off by default, kept in localStorage,
// and the patch set the page prepares follows it.
import assert from 'node:assert/strict';
import { beforeEach, test } from 'vitest';

const store = new Map<string, string>();
Object.defineProperty(globalThis, 'localStorage', {
  value: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
  },
  configurable: true,
});

beforeEach(() => store.clear());

test('off by default: the page patches the device set', async () => {
  const s = await import('../src/firmware/default-modules-state.ts');
  assert.equal(s.customBootScreen(), false);
  assert.equal(s.patchSet(), 'device');
});

test('on: the introblank set, and the choice is kept', async () => {
  const s = await import('../src/firmware/default-modules-state.ts');
  s.setCustomBootScreen(true);
  assert.equal(s.patchSet(), 'device-introblank');
  assert.equal(store.get('digikit.customBootScreen'), '1');
  s.setCustomBootScreen(false);
  assert.equal(s.patchSet(), 'device');
  assert.equal(store.get('digikit.customBootScreen'), '0');
});
