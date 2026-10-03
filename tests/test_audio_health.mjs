import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import { hostMetrics, pcmMetrics } from '../packages/web/src/runtime-metrics.ts';

function processor() {
  let Processor;
  const reports = [];
  const context = { sampleRate: 48000, Float32Array,
    AudioWorkletProcessor: class { constructor() { this.port = { postMessage: (value) => reports.push(value) }; } },
    registerProcessor: (_name, ctor) => { Processor = ctor; },
  };
  vm.runInNewContext(fs.readFileSync(new URL('../packages/web/public/pcm-worklet.js', import.meta.url), 'utf8'), context);
  const p = new Processor();
  const send = (data) => p.port.onmessage({ data });
  const render = (n) => {
    const left = new Float32Array(n), right = new Float32Array(n);
    assert.equal(p.process([], [[left, right]]), true);
    return [Array.from(left), Array.from(right)];
  };
  return { p, send, render, reports };
}

test('PCM rate is emitted stereo duration over this session wall time', () => {
  assert.deepEqual(pcmMetrics(96000, 2000), { pcm_produced_seconds: 1, production_elapsed_wall_seconds: 2, pcm_seconds_per_wall_second: 0.5 });
  assert.equal(pcmMetrics(0, 0).pcm_seconds_per_wall_second, 0);
});

test('host response histogram is bounded, resettable and exported by value', () => {
  const m = hostMetrics('wasm_abi'); m.reset();
  const snapshot = { status: { ready: false } };
  for (const n of [0, 1, 2, 5, 10, 20, 50, 100, 101]) m.step(0, n, snapshot);
  const report = m.report();
  assert.deepEqual(report.step_response_wall_ms_buckets.counts, [2, 1, 1, 1, 1, 1, 1, 1]);
  assert.equal(report.chunks, 9);
  m.reset();
  assert.deepEqual(m.report().step_response_wall_ms_buckets.counts, Array(8).fill(0));
  assert.equal(report.step_response_wall_ms_buckets.counts[0], 2);
});

test('buffer threshold and stereo output are unchanged; initial silence is not underrun', () => {
  const { p, send, render } = processor();
  send({ type: 'config', startSeconds: 4 / 48000 });
  send(new Float32Array([1, 2, 3, 4]));
  assert.deepEqual(render(2), [[0, 0], [0, 0]]);
  assert.equal(p.silence, 2); assert.equal(p.underrunFrames, 0);
  send(new Float32Array([5, 6, 7, 8]));
  assert.deepEqual(render(4), [[1, 3, 5, 7], [2, 4, 6, 8]]);
  assert.equal(p.highWater, 4); assert.equal(p.played, 4);
});

test('underrun duration includes the partial quantum and subsequent rebuffering', () => {
  const { p, send, render, reports } = processor();
  send({ type: 'config', startSeconds: 0 });
  const port = {}; send({ port });
  port.onmessage({ data: new Float32Array([1, 2, 3, 4]) });
  assert.deepEqual(render(4), [[1, 3, 0, 0], [2, 4, 0, 0]]);
  render(4);
  assert.equal(p.underruns, 1); assert.equal(p.underrunFrames, 6);
  p.sinceReport = 12000; render(4);
  const report = reports.at(-1);
  assert.equal(report.underrunSeconds, 10 / 48000);
  assert.equal(report.highWaterSeconds, 2 / 48000);
  assert.equal(report.portAttached, true);
  assert.equal(report.receivedPcm, true); assert.equal(report.renderedPcm, true);
  send({ type: 'flush' }); assert.equal(p.queued, 0); assert.equal(p.playing, false);
});
