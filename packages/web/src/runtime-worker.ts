/// <reference lib="webworker" />

type Abi = Record<string, CallableFunction>;
let wasm: Abi | undefined;
let corePromise: Promise<Abi> | undefined;
let generation = 0;
let runEpoch = 0;
let running = false;
let serial = Promise.resolve();

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
function stopCore() { if (!wasm) return; wasm.digi_stop(); result(); }
function core() {
  corePromise ??= WebAssembly.instantiateStreaming(fetch('/emulator-core.wasm'), {}).then(({ instance }) => wasm = instance.exports as unknown as Abi);
  return corePromise;
}
function publish(snapshot: unknown, token: number) { postMessage({ type: 'snapshot', generation: token, snapshot }); }
async function pump(token: number, epoch: number) {
  if (!running || token !== generation || epoch !== runEpoch) return;
  try { const snapshot = call('digi_step', 250_000); publish(snapshot, token); if (snapshot.status?.error) { running = false; postMessage({ type: 'error', generation: token, error: snapshot.status.error }); return; } } catch (error) { running = false; postMessage({ type: 'error', generation: token, error: String(error) }); return; }
  setTimeout(() => { void pump(token, epoch); }, 0);
}
function reject(data: { id?: string }, error: string) { if (data.id) postMessage({ reply: data.id, error }); }
async function handle(data: { type: string; id?: string; generation: number; bytes?: ArrayBuffer; code?: number; down?: boolean; encoder?: number; detents?: number }) {
  await core();
  if (data.type === 'load') {
    if (data.generation < generation) return reject(data, 'stale emulator session');
    generation = data.generation; running = false; runEpoch += 1; stopCore();
    if (!data.bytes) return reject(data, 'firmware bytes are missing');
    const bytes = new Uint8Array(data.bytes);
    const pointer = wasm!.digi_alloc(bytes.length) as number;
    if (pointer === 0 && bytes.length !== 0) return reject(data, 'native allocation failed');
    try {
        new Uint8Array((wasm!.memory as unknown as WebAssembly.Memory).buffer, pointer, bytes.length).set(bytes);
      const snapshot = call('digi_load', pointer, bytes.length);
      publish(snapshot, generation); running = true; const epoch = ++runEpoch; setTimeout(() => { void pump(generation, epoch); }, 0);
      if (data.id) postMessage({ reply: data.id, value: snapshot });
    } finally { wasm!.digi_dealloc(pointer, bytes.length); }
    return;
  }
  if (data.type === 'stop') {
    if (data.generation < generation) return reject(data, 'stale emulator session');
    generation = data.generation; running = false; runEpoch += 1; stopCore(); if (data.id) postMessage({ reply: data.id, value: undefined }); return;
  }
  if (data.generation !== generation) return reject(data, 'stale emulator session');
  if (data.type === 'pause') { running = false; runEpoch += 1; return; }
  if (data.type === 'resume') { if (!running) { running = true; const epoch = ++runEpoch; setTimeout(() => { void pump(generation, epoch); }, 0); } return; }
  if (data.type === 'button') { call('digi_button', data.code!, data.down ? 1 : 0); if (data.id) postMessage({ reply: data.id, value: undefined }); return; }
  if (data.type === 'turn') { call('digi_turn', data.encoder!, data.detents!); if (data.id) postMessage({ reply: data.id, value: undefined }); return; }
}
self.onmessage = ({ data }) => { serial = serial.then(() => handle(data)).catch((error) => reject(data, String(error))); };
