// The bar's speed wiring (apps/web/src/transfer/bar-rate.ts) under real Solid effects. The page hands the bar a new object on every progress
// event (`<Show when={bar()}>`), so `running` notifies with an unchanged value: the samples must survive that.
import assert from 'node:assert/strict';
import { createRoot, createSignal } from 'solid-js';
import { test } from 'vitest';
import { createBarRate } from '../../../web/src/transfer/bar-rate.ts';

interface Bar {
  sent: number;
  running: boolean;
}

function setup() {
  let t = 1_000_000;
  const [bar, setBar] = createSignal<Bar>({ sent: 0, running: false }, { equals: false });
  let dispose = () => {};
  const speed = createRoot((d) => {
    dispose = d;
    return createBarRate(
      () => bar().sent,
      () => bar().running,
      undefined,
      () => t,
      1e9,
    );
  });
  return {
    speed,
    dispose,
    event: (dt: number, b: Bar) => {
      t += dt;
      setBar({ ...b });
    },
  };
}

test('a new bar object on every event keeps the samples, and a speed appears', () => {
  const s = setup();
  s.event(0, { sent: 0, running: true });
  for (let i = 1; i <= 10; i++) s.event(200, { sent: i * 100_000, running: true });
  assert.ok(Math.abs((s.speed() ?? 0) - 500_000) < 1, `speed ${s.speed()}`);
  s.dispose();
});

test('running turning false then true starts a new measurement', () => {
  const s = setup();
  s.event(0, { sent: 0, running: true });
  for (let i = 1; i <= 5; i++) s.event(200, { sent: i * 100_000, running: true });
  assert.ok(s.speed() !== undefined);
  s.event(200, { sent: 500_000, running: false });
  assert.equal(s.speed(), undefined);
  s.event(5000, { sent: 500_000, running: true });
  assert.equal(s.speed(), undefined, 'one sample after the reset says nothing');
  s.event(500, { sent: 600_000, running: true });
  assert.ok(Math.abs((s.speed() ?? 0) - 200_000) < 1, `speed ${s.speed()}`);
  s.dispose();
});

test('a loop of small files with a pause for END and VERIFY after each keeps a steady speed', () => {
  const s = setup();
  s.event(0, { sent: 0, running: true });
  const seen: number[] = [];
  for (let i = 1; i <= 30; i++) {
    s.event(100, { sent: i * 20_000, running: true }); // DATA of one 20 KB file
    s.event(900, { sent: i * 20_000, running: true }); // END and VERIFY: no new bytes
    if (i > 8) seen.push(s.speed() ?? -1);
  }
  // 20 KB per second on average; the window never empties
  for (const v of seen) assert.ok(v > 15_000 && v < 25_000, `speed ${v}`);
  s.dispose();
});
