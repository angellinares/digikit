import { hostMetrics } from './runtime-metrics';
export interface RuntimeStatus {
  device: string; version: string; icount: number; pc: number; ready: boolean; phase: string;
  error: string | null; frame_revision: number; frame_source: string | null;
  main_ui_reached: boolean; filesystem_verified: boolean | null; input_ready: boolean;
  input_pending: number; input_irqs: number;
}
export interface RuntimeSnapshot { status: RuntimeStatus; frame?: Uint8Array }
export interface RuntimeDiagnostics {
  schema_version: number; device: string; version: string;
  sharc_execution_connected: boolean; pcm_output_connected: boolean;
  [key: string]: unknown;
}
export interface NativeAudioStatus {
  sink_connected: boolean; playback_requested: boolean; flow_started: boolean;
  setup_needed?: boolean;
  source_audio_seconds: number; session_wall_seconds: number;
  output_device?: string;
  device_error: string | null;
  sink?: {
    stream_started: boolean; source_received_frames: number; queued_device_frames: number;
    underrun_events: number; error: string | null;
  } | null;
}
/** Dev-only inputs of the coupled core: user-supplied, never bundled. */
export interface CoupledFiles { syx: Uint8Array; image: Uint8Array; dsp: Uint8Array; snapshot?: Uint8Array }
export interface CoupledStats { frames: number; dsp_instructions: number; halted: string | null; nonzero_replies: number; missing_blocks: number; pcm_values: number; pcm_produced_seconds: number; production_elapsed_wall_seconds: number; pcm_seconds_per_wall_second: number; dsp_attached: boolean; pcm_port_connected: boolean; host: Record<string, unknown> }
export interface EmulatorRuntime {
  /** Browser worker only, and only with a core built with the `sharc` feature. */
  loadCoupled?(files: CoupledFiles, audioPort: MessagePort): Promise<RuntimeSnapshot>;
  coupledAvailable?(): Promise<boolean>;
  coupledStats?(): Promise<CoupledStats>;
  /** Native desktop only: observational status for its default output sink. */
  nativeAudio?(): Promise<NativeAudioStatus | undefined>;
  /** Press a key, release it after `hold` guest instructions (emulation time, not wall time). */
  tap?(code: number, hold?: number): Promise<void>;
  diagnostics(): Promise<RuntimeDiagnostics>;
  load(bytes: Uint8Array, name: string): Promise<RuntimeSnapshot>; restart(): Promise<RuntimeSnapshot>;
  pause(): void; resume(): void; press(code: number): Promise<void>; release(code: number): Promise<void>;
  turn(encoder: number, detents: number): Promise<void>; stop(): Promise<void>; dispose(): void;
}
export type RuntimeUpdate = { type: 'snapshot'; snapshot: RuntimeSnapshot } | { type: 'error'; error: string };

export const drawFrame = (canvas: HTMLCanvasElement, frame: Uint8Array) => {
  if (frame.length !== 1024) return;
  const context = canvas.getContext('2d'); if (!context) return;
  const pixels = context.createImageData(128, 64);
  for (let y = 0; y < 64; y += 1) for (let x = 0; x < 128; x += 1) {
    const on = (frame[(7 - Math.floor(y / 8)) + 8 * x] & (1 << (y % 8))) !== 0;
    const at = (y * 128 + x) * 4;
    pixels.data[at] = on ? 242 : 8; pixels.data[at + 1] = on ? 244 : 9;
    pixels.data[at + 2] = on ? 249 : 12; pixels.data[at + 3] = 255;
  }
  context.putImageData(pixels, 0, 0);
};

export function browserRuntime(onUpdate: (update: RuntimeUpdate) => void): EmulatorRuntime {
  const worker = new Worker(new URL('./runtime-worker.ts', import.meta.url), { type: 'module' });
  let generation = 0, paused = true, source: Uint8Array | undefined, name = '';
  worker.onmessage = ({ data }: MessageEvent<RuntimeUpdate & { generation?: number }>) => { if (data.generation === generation) onUpdate(data); };
  const request = <T>(type: string, payload: Record<string, unknown> = {}) => new Promise<T>((resolve, reject) => {
    const id = crypto.randomUUID();
    const listener = ({ data }: MessageEvent<{ reply?: string; value?: T; error?: string }>) => {
      if (data.reply !== id) return; worker.removeEventListener('message', listener);
      data.error ? reject(new Error(data.error)) : resolve(data.value as T);
    };
    worker.addEventListener('message', listener); worker.postMessage({ type, id, generation, ...payload });
  });
  return {
    diagnostics: () => request<RuntimeDiagnostics>('diagnostics'),
    async loadCoupled(files, audioPort) {
      generation += 1; paused = false; source = undefined;
      worker.postMessage({ type: 'audio-port', generation, port: audioPort }, [audioPort]);
      const copy = (bytes?: Uint8Array) => bytes?.slice().buffer;
      return request<RuntimeSnapshot>('load-coupled', { bytes: copy(files.syx), image: copy(files.image), dsp: copy(files.dsp), snapshot: copy(files.snapshot) });
    },
    coupledStats: () => request<CoupledStats>('coupled-stats'),
    coupledAvailable: () => request<boolean>('coupled-capable'),
    tap: (code, hold) => request<void>('tap', { code, hold }),
    async load(bytes, selectedName) { generation += 1; paused = false; source = bytes.slice(); name = selectedName; return request<RuntimeSnapshot>('load', { bytes: bytes.buffer, name: selectedName }); },
    async restart() { if (!source) throw new Error('No firmware selected'); generation += 1; paused = false; return request<RuntimeSnapshot>('load', { bytes: source.slice().buffer, name }); },
    pause() { paused = true; worker.postMessage({ type: 'pause', generation }); },
    resume() { if (!paused) return; paused = false; worker.postMessage({ type: 'resume', generation }); },
    press: (code) => request<void>('button', { code, down: true }), release: (code) => request<void>('button', { code, down: false }),
    turn: (encoder, detents) => request<void>('turn', { encoder, detents }),
    async stop() { generation += 1; paused = true; source = undefined; worker.postMessage({ type: 'stop', generation }); }, dispose() { generation += 1; paused = true; worker.terminate(); },
  };
}

export async function nativeRuntime(onUpdate: (update: RuntimeUpdate) => void): Promise<EmulatorRuntime | undefined> {
  let api: typeof import('@tauri-apps/api/core'); try { api = await import('@tauri-apps/api/core'); } catch { return undefined; }
  if (!api.isTauri()) return undefined;
  const metrics = hostMetrics('native_ipc');
  let generation = 0, activeSession: number | undefined, loaded = false, paused = true, disposed = false, epoch = 0, timer: number | undefined, input = Promise.resolve(), lifecycle = Promise.resolve();
  const clearPump = () => { paused = true; epoch += 1; if (timer) window.clearTimeout(timer); timer = undefined; };
  const fault = (error: unknown) => { clearPump(); loaded = false; onUpdate({ type: 'error', error: String(error) }); };
  const pump = async (token: number, run: number) => {
    if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
    await input.catch(() => {});
    if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
    try {
      const begin = performance.now(); const snapshot = await api.invoke<RuntimeSnapshot>('emu_step', { sessionId: token, budget: 250_000 });
      if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
      metrics.step(begin, performance.now(), snapshot); onUpdate({ type: 'snapshot', snapshot });
      if (snapshot.status.error) { fault(snapshot.status.error); return; }
      timer = window.setTimeout(() => { void pump(token, run); }, 0);
    } catch (error) { if (token === generation && run === epoch) fault(error); }
  };
  const start = (token: number) => { paused = false; const run = ++epoch; timer = window.setTimeout(() => { void pump(token, run); }, 0); };
  const queueInput = (operation: (session: number) => Promise<void>) => {
    const token = generation, session = activeSession;
    const next = input.catch(() => {}).then(async () => { if (!disposed && loaded && token === generation && session === activeSession && session !== undefined) await operation(session); });
    input = next; return next.catch((error) => { if (token === generation) fault(error); throw error; });
  };
  try {
    metrics.reset(); const startup = await api.invoke<RuntimeSnapshot>('emu_startup'); metrics.loaded();
    generation = 1; activeSession = 1; loaded = true; onUpdate({ type: 'snapshot', snapshot: startup }); start(1);
  } catch (error) {
    if (!String(error).includes('No startup firmware selected')) onUpdate({ type: 'error', error: String(error) });
  }
  return {
    async diagnostics() { const session = activeSession; if (session === undefined) throw new Error('No firmware selected'); const report = await api.invoke<RuntimeDiagnostics>('emu_diagnostics', { sessionId: session }); if (disposed || activeSession !== session) throw new Error('Firmware session changed during diagnostic export'); return { ...report, host: metrics.report() }; },
    async nativeAudio() {
      const session = activeSession;
      if (session === undefined) return undefined;
      const report = await api.invoke<RuntimeDiagnostics & { native_audio?: NativeAudioStatus }>('emu_diagnostics', { sessionId: session });
      if (disposed || activeSession !== session) throw new Error('Firmware session changed while reading audio status');
      return report.native_audio ?? {
        sink_connected: false,
        playback_requested: false,
        flow_started: false,
        source_audio_seconds: 0,
        session_wall_seconds: 0,
        device_error: null,
        setup_needed: true,
      };
    },
    async load(bytes, _name) { const token = ++generation; clearPump(); loaded = false; activeSession = undefined; const work = lifecycle.catch(() => {}).then(async () => { metrics.reset(); const snapshot = await api.invoke<RuntimeSnapshot>('emu_load', bytes, { headers: { 'x-session-id': String(token) } }); if (disposed || token !== generation) return snapshot; metrics.loaded(); activeSession = token; loaded = true; onUpdate({ type: 'snapshot', snapshot }); start(token); return snapshot; }); lifecycle = work.then(() => {}, () => {}); return work.catch((error) => { if (token === generation) fault(error); throw error; }); },
    async restart() { const old = activeSession; if (!loaded || old === undefined) throw new Error('No firmware selected'); const token = ++generation; clearPump(); loaded = false; activeSession = undefined; const work = lifecycle.catch(() => {}).then(async () => { metrics.reset(); const snapshot = await api.invoke<RuntimeSnapshot>('emu_restart', { sessionId: old, nextSessionId: token }); if (disposed || token !== generation) return snapshot; metrics.loaded(); activeSession = token; loaded = true; onUpdate({ type: 'snapshot', snapshot }); start(token); return snapshot; }); lifecycle = work.then(() => {}, () => {}); return work.catch((error) => { if (token === generation) fault(error); throw error; }); },
    pause() { clearPump(); }, resume() { if (loaded && activeSession !== undefined && paused) start(activeSession); },
    press: (code) => queueInput((session) => api.invoke<void>('emu_button', { sessionId: session, code, down: true })), release: (code) => queueInput((session) => api.invoke<void>('emu_button', { sessionId: session, code, down: false })), turn: (encoder, detents) => queueInput((session) => api.invoke<void>('emu_turn', { sessionId: session, encoder, detents })),
    tap: (code, hold) => {
      const token = generation;
      return queueInput(async (session) => {
        const snapshot = await api.invoke<RuntimeSnapshot>('emu_tap', { sessionId: session, code, hold });
        if (!disposed && token === generation && activeSession === session) onUpdate({ type: 'snapshot', snapshot });
      });
    },
    async stop() { const token = ++generation; clearPump(); loaded = false; activeSession = undefined; await (lifecycle = lifecycle.catch(() => {}).then(() => api.invoke<void>('emu_stop', { sessionId: token }))); }, dispose() { disposed = true; const token = ++generation; clearPump(); loaded = false; activeSession = undefined; void api.invoke<void>('emu_stop', { sessionId: token }); },
  };
}
