/// <reference lib="webworker" />

import { hostMetrics, pcmMetrics } from './runtime-metrics';
const metrics = hostMetrics('wasm_abi');

type Abi = Record<string, CallableFunction>;
let wasm: Abi | undefined;
let plain: Abi | undefined;
let corePromise: Promise<Abi> | undefined;
let sharcPromise: Promise<Abi> | undefined;
let generation = 0;
let runEpoch = 0;
let running = false;
let serial = Promise.resolve();
// Coupled ColdFire + SHARC+ audio (dev path, `sharc` build of the core).
let coupled = false;
let audioPort: MessagePort | undefined;
let pcmValues = 0;
let audioStartedAt = 0;
let lastIcount = 0;
const pendingRelease: { code: number; at: number }[] = [];
// Executable ranges the developer declares for the runaway check (text form,
// "0x4670c000-0x4670ef48, ..."), applied to every emulator this worker loads.
let execRanges = '';
// A coupled step runs ~3 DSP frames, so loop steps inside one task (setTimeout
// would clamp to 4 ms between them) and yield about every 20 ms for input.
const COUPLED_SLICE_MS = 20;

function result() {
  if (!wasm) throw new Error('WASM core is not loaded');
  const pointer = wasm.digi_result_ptr() as number;
  const length = wasm.digi_result_len() as number;
  return JSON.parse(new TextDecoder().decode(new Uint8Array((wasm.memory as unknown as WebAssembly.Memory).buffer, pointer, length)));
}
function call(name: string, ...args: number[]) {
  if (!wasm) throw new Error('WASM core is not loaded');
  const code = wasm[name](...args) as number;
  const value = result();
  if (code !== 0 || value.error) throw new Error(value.error ?? `native error ${code}`);
  return value.snapshot ?? value;
}
function stopCore() { if (!wasm) return; wasm.digi_stop(); result(); coupled = false; pendingRelease.length = 0; }
function memory() { return (wasm!.memory as unknown as WebAssembly.Memory).buffer; }
/** Move the PCM produced since the last call to the audio port. */
function sendPcm() {
  const count = wasm!.digi_pcm_take() as number;
  if (count === 0 || !audioPort) return;
  const block = new Float32Array(memory(), wasm!.digi_pcm_ptr() as number, count).slice();
  pcmValues += count;
  audioPort.postMessage(block, [block.buffer]);
}
/** On-demand observational report; no stepping or new device reads. */
function coupledReport() {
  return { ...call('digi_sharc_stats'), pcm_values: pcmValues, ...pcmMetrics(pcmValues, performance.now() - audioStartedAt), dsp_attached: coupled, pcm_port_connected: Boolean(audioPort), host: metrics.report() };
}
/** Copy a page-supplied buffer into the module; returns its pointer. */
function put(bytes: Uint8Array) {
  const pointer = wasm!.digi_alloc(bytes.length) as number;
  if (pointer === 0 && bytes.length !== 0) throw new Error('native allocation failed');
  new Uint8Array(memory(), pointer, bytes.length).set(bytes);
  return pointer;
}
/** Hand the declared executable ranges to the loaded emulator (empty text clears them). */
function applyExecRanges() {
  if (!wasm?.digi_exec_ranges) { if (execRanges.trim()) throw new Error('this emulator core predates declared executable ranges: rebuild it'); return; }
  const bytes = new TextEncoder().encode(execRanges); const pointer = put(bytes);
  try { call('digi_exec_ranges', pointer, bytes.length); } finally { wasm!.digi_dealloc(pointer, bytes.length); }
}
function core() {
  corePromise ??= WebAssembly.instantiateStreaming(fetch('/emulator-core.wasm'), {}).then(({ instance }) => { plain = instance.exports as unknown as Abi; wasm ??= plain; return plain; });
  return corePromise;
}
/** The ordinary core includes the local SHARC engine when it was built with
 * SHARC_GEN_DIR. The legacy separate file remains a fallback for old builds. */
async function sharcCore() {
  const ordinary = await core();
  if (typeof ordinary.digi_load_coupled === 'function') return ordinary;
  sharcPromise ??= WebAssembly.instantiateStreaming(fetch('/emulator-core-sharc.wasm'), {}).then(({ instance }) => instance.exports as unknown as Abi);
  return sharcPromise;
}
function publish(snapshot: unknown, token: number) { postMessage({ type: 'snapshot', generation: token, snapshot }); }
function step() {
  const snapshot = call('digi_step', 250_000);
  lastIcount = snapshot.status.icount;
  if (coupled) sendPcm();
  for (let i = pendingRelease.length - 1; i >= 0; i -= 1) {
    if (snapshot.status.icount >= pendingRelease[i].at) { call('digi_button', pendingRelease[i].code, 0); pendingRelease.splice(i, 1); }
  }
  return snapshot;
}
async function pump(token: number, epoch: number) {
  if (!running || token !== generation || epoch !== runEpoch) return;
  try {
    const begin = performance.now(); let snapshot = step(); metrics.step(begin, performance.now(), snapshot);
    while (coupled && !snapshot.status?.error && performance.now() - begin < COUPLED_SLICE_MS) { const t = performance.now(); snapshot = step(); metrics.step(t, performance.now(), snapshot); }
    publish(snapshot, token);
    if (snapshot.status?.error) { running = false; postMessage({ type: 'error', generation: token, error: snapshot.status.error }); return; }
  } catch (error) { running = false; postMessage({ type: 'error', generation: token, error: String(error) }); return; }
  setTimeout(() => { void pump(token, epoch); }, 0);
}
function reject(data: { id?: string }, error: string) { if (data.id) postMessage({ reply: data.id, error }); }
type Message = { type: string; id?: string; generation: number; bytes?: ArrayBuffer; image?: ArrayBuffer; dsp?: ArrayBuffer; snapshot?: ArrayBuffer; port?: MessagePort; text?: string; hold?: number; code?: number; down?: boolean; encoder?: number; detents?: number };
async function handle(data: Message) {
  await core();
  if (data.type === 'audio-port') { audioPort = data.port; return; }
  if (data.type === 'coupled-capable') {
    try {
      const candidate = await sharcCore();
      if (data.id) postMessage({ reply: data.id, value: typeof candidate.digi_load_coupled === 'function' });
    } catch {
      if (data.id) postMessage({ reply: data.id, value: false });
    }
    return;
  }
  if (data.type === 'exec-ranges') {
    execRanges = data.text ?? '';
    // applied now when an emulator is running, and to every later load
    try { if (wasm) applyExecRanges(); }
    catch (error) { if (!String(error).includes('no emulator is loaded')) return reject(data, String(error)); }
    if (data.id) postMessage({ reply: data.id, value: undefined }); return;
  }
  if (data.type === 'load') {
    if (data.generation < generation) return reject(data, 'stale emulator session');
    generation = data.generation; running = false; runEpoch += 1; stopCore(); wasm = plain;
    if (!data.bytes) return reject(data, 'firmware bytes are missing');
    const bytes = new Uint8Array(data.bytes);
    const pointer = wasm!.digi_alloc(bytes.length) as number;
    if (pointer === 0 && bytes.length !== 0) return reject(data, 'native allocation failed');
    try {
        new Uint8Array((wasm!.memory as unknown as WebAssembly.Memory).buffer, pointer, bytes.length).set(bytes);
      metrics.reset(); const snapshot = call('digi_load', pointer, bytes.length); applyExecRanges(); metrics.loaded();
      publish(snapshot, generation); running = true; const epoch = ++runEpoch; setTimeout(() => { void pump(generation, epoch); }, 0);
      if (data.id) postMessage({ reply: data.id, value: snapshot });
    } finally { wasm!.digi_dealloc(pointer, bytes.length); }
    return;
  }
  if (data.type === 'load-coupled') {
    if (data.generation < generation) return reject(data, 'stale emulator session');
    generation = data.generation; running = false; runEpoch += 1; stopCore();
    try { wasm = await sharcCore(); } catch (error) { wasm = plain; return reject(data, `Audio setup needs a browser core built with SHARC support: ${error}`); }
    if (typeof wasm.digi_load_coupled !== 'function') { wasm = plain; return reject(data, 'Audio setup needs a browser core built with SHARC support.'); }
    if (!data.bytes || !data.image || !data.dsp) return reject(data, 'firmware, DSP image and DSP state are required');
    const parts = [new Uint8Array(data.bytes), new Uint8Array(data.image), new Uint8Array(data.dsp), new Uint8Array(data.snapshot ?? new ArrayBuffer(0))];
    const pointers: number[] = [];
    try {
      for (const part of parts) pointers.push(put(part));
      metrics.reset();
      const snapshot = call('digi_load_coupled', ...parts.flatMap((part, i) => [pointers[i], part.length])); applyExecRanges();
      metrics.loaded(); coupled = true; pcmValues = 0; audioStartedAt = performance.now();
      publish(snapshot, generation); running = true; const epoch = ++runEpoch; setTimeout(() => { void pump(generation, epoch); }, 0);
      if (data.id) postMessage({ reply: data.id, value: snapshot });
    } finally { parts.forEach((part, i) => { if (i < pointers.length) wasm!.digi_dealloc(pointers[i], part.length); }); }
    return;
  }
  if (data.type === 'stop') {
    if (data.generation < generation) return reject(data, 'stale emulator session');
    generation = data.generation; running = false; runEpoch += 1; stopCore(); if (data.id) postMessage({ reply: data.id, value: undefined }); return;
  }
  if (data.generation !== generation) return reject(data, 'stale emulator session');
  if (data.type === 'coupled-stats') { if (!coupled) return reject(data, 'not a coupled session'); if (data.id) postMessage({ reply: data.id, value: coupledReport() }); return; }
  if (data.type === 'tap') { call('digi_button', data.code!, 1); pendingRelease.push({ code: data.code!, at: lastIcount + (data.hold ?? 40_000_000) }); if (data.id) postMessage({ reply: data.id, value: undefined }); return; }
  if (data.type === 'diagnostics') { const report = { ...call('digi_diagnostics'), host: metrics.report(), audio: coupled ? coupledReport() : undefined }; if (data.id) postMessage({ reply: data.id, value: report }); return; }
  if (data.type === 'pause') { running = false; runEpoch += 1; return; }
  if (data.type === 'resume') { if (!running) { running = true; const epoch = ++runEpoch; setTimeout(() => { void pump(generation, epoch); }, 0); } return; }
  if (data.type === 'button') { call('digi_button', data.code!, data.down ? 1 : 0); if (data.id) postMessage({ reply: data.id, value: undefined }); return; }
  if (data.type === 'turn') { call('digi_turn', data.encoder!, data.detents!); if (data.id) postMessage({ reply: data.id, value: undefined }); return; }
}
self.onmessage = ({ data }) => { serial = serial.then(() => handle(data)).catch((error) => reject(data, String(error))); };
