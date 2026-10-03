import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import { hostMetrics, pcmMetrics } from '../packages/web/src/runtime-metrics.ts';
const require = createRequire(import.meta.url);
const ts = require('../packages/web/node_modules/typescript/lib/typescript.js');

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

function workerHarness({ ordinaryHasCoupling = false } = {}) {
  let result = {};
  let icount = 0;
  const buttons = [];
  const timers = [];
  const replies = new Map();
  const memory = new WebAssembly.Memory({ initial: 1 });
  const writeResult = (value) => {
    result = value;
    const bytes = new TextEncoder().encode(JSON.stringify(value));
    new Uint8Array(memory.buffer).set(bytes, 0);
    return 0;
  };
  const abi = {
    memory,
    digi_result_ptr: () => 0,
    digi_result_len: () => new TextEncoder().encode(JSON.stringify(result)).length,
    digi_stop: () => writeResult({}),
    digi_alloc: () => 8,
    digi_dealloc: () => {},
    digi_load: () => writeResult({ snapshot: { status: { icount } } }),
    digi_step: () => { icount += 100; return writeResult({ snapshot: { status: { icount } } }); },
    digi_button: (code, down) => { buttons.push([code, down]); return writeResult({}); },
  };
  if (ordinaryHasCoupling) abi.digi_load_coupled = () => writeResult({ snapshot: { status: { icount } } });
  const source = fs.readFileSync(new URL('../packages/web/src/runtime-worker.ts', import.meta.url), 'utf8')
    .replace("import { hostMetrics, pcmMetrics } from './runtime-metrics';", 'const hostMetrics = () => ({ reset() {}, loaded() {}, step() {}, report() { return {}; } }); const pcmMetrics = () => ({});');
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const context = {
    WebAssembly: { Memory: WebAssembly.Memory, instantiateStreaming: async (response) => {
      await response;
      return { instance: { exports: abi } };
    } },
    fetch: async (url) => {
      if (String(url).includes('sharc')) throw new Error('missing sharc core');
      return {};
    },
    TextDecoder, TextEncoder, Uint8Array, Float32Array, performance: { now: () => 0 },
    setTimeout: (fn) => { timers.push(fn); return timers.length; },
    postMessage: (message) => { if (message.reply) replies.get(message.reply)?.(message); },
    self: {}, console,
  };
  vm.runInNewContext(js, context);
  const send = (data) => new Promise((resolve, reject) => {
    replies.set(data.id, (message) => {
      replies.delete(data.id);
      message.error ? reject(new Error(message.error)) : resolve(message.value);
    });
    context.self.onmessage({ data });
  });
  return { send, buttons, runTimer: async () => { const timer = timers.shift(); if (timer) await timer(); } };
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

test('missing SHARC exports report unavailable capability before any coupled load', async () => {
  const worker = workerHarness();
  assert.equal(await worker.send({ type: 'coupled-capable', id: 'capability', generation: 0 }), false);
  assert.deepEqual(worker.buttons, []);
});

test('a deferred tap releases after an ordinary CF step', async () => {
  const worker = workerHarness();
  await worker.send({ type: 'load', id: 'load', generation: 1, bytes: new Uint8Array([1]).buffer });
  await worker.send({ type: 'tap', id: 'tap', generation: 1, code: 25, hold: 100 });
  await worker.runTimer();
  assert.deepEqual(worker.buttons, [[25, 1], [25, 0]]);
});
