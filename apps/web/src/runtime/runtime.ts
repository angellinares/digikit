import { hostMetrics } from './runtime-metrics';
import { createLinkBuffer, LinkServer } from '../sab-link.ts';
import type { DspStart } from '../dsp-worker.ts';
import type { DspStats, FastDefault, FastRegions } from '../split-session.ts';
import fastRegionTable from '../../../../config/emulator.json';
import { sha256Hex } from './local-inputs';
import type { CardBridge, CardStatus } from './cards';
import type { ReloadReport } from './reload';

const base = import.meta.env.BASE_URL.replace(/\/?$/, '/');

const pcs = (list: string | undefined) =>
  (list ?? '')
    .split(',')
    .filter((x) => x.trim())
    .map((x) => parseInt(x.trim(), 16));

/**
 * The DSP fast tier of the page (dev page, `?fast=`). Without a query the DSP worker decides from the
 * core (a `FastDefault`): `4,2,1` when the core is generic (`digi_sharc_aot_blocks() === 0`); otherwise
 * `desktop` when config/emulator.json has an entry for the firmware; otherwise off. `?fast=off` disables it; `desktop` is the desktop
 * configuration (the static regions and cluster roots of config/emulator.json for this
 * firmware); `auto` or `T,M,F` is run-time selection (threshold, min insns, follow); `desktop+auto`
 * both. A firmware without an entry in the table gets no static regions.
 */
export async function fastConfig(
  syx: Uint8Array,
  query = new URLSearchParams(location.search).get('fast'),
): Promise<[number, number, number] | FastRegions | FastDefault | undefined> {
  if (query === '' || query === 'off') return undefined;
  const table = fastRegionTable as Record<string, { fast_regions?: string; blocktrans?: string }>;
  const entry = table[await sha256Hex(syx)];
  // `m || 64` makes M=0 impossible.
  const auto = (text: string): [number, number, number] => {
    const [t, m, f] = text.split(',').map(Number);
    return [t || 256, m || 64, f || 0];
  };
  if (query === 'auto') return [256, 64, 0];
  if (query !== null && !query.startsWith('desktop')) return auto(query);
  const config: FastRegions = { regions: pcs(entry?.fast_regions), roots: pcs(entry?.blocktrans) };
  if (query === null) return entry ? { kind: 'default', desktop: config } : { kind: 'default' };
  if (query.startsWith('desktop+auto')) config.auto = [256, 64];
  return config;
}
export interface RuntimeStatus {
  device: string;
  version: string;
  icount: number;
  pc: number;
  ready: boolean;
  phase: string;
  error: string | null;
  frame_revision: number;
  frame_source: string | null;
  main_ui_reached: boolean;
  filesystem_verified: boolean | null;
  input_ready: boolean;
  input_pending: number;
  input_irqs: number;
  led_revision?: number;
}
/** `leds`: 64 panel LED values as 128 hex digits; `rgb`: 64 x (r,g,b) as 384 hex digits. Both only when `status.led_revision` changed. */
export interface RuntimeSnapshot {
  status: RuntimeStatus;
  frame?: Uint8Array;
  leds?: string;
  rgb?: string;
  polled?: boolean;
}
export interface RuntimeDiagnostics {
  schema_version: number;
  device: string;
  version: string;
  sharc_execution_connected: boolean;
  pcm_output_connected: boolean;
  [key: string]: unknown;
}
export interface NativeAudioStatus {
  sink_connected: boolean;
  playback_requested: boolean;
  flow_started: boolean;
  setup_needed?: boolean;
  source_audio_seconds: number;
  session_wall_seconds: number;
  output_device?: string;
  device_error: string | null;
  audio_seconds_per_session_wall_second?: number;
  audio_mode?: string;
  halted?: string | null;
  /** CPU meters of the last window, percent (docs/CONTROL.md `status.load`); the `real_*` values are estimates. */
  load?: {
    window_s: number;
    cf: { model_pct: number; real_pct: number; real_high_pct: number };
    dsp: {
      busy_per_block: number;
      model_pct: number;
      real_pct: number;
      real_high_pct: number;
      peak_pct?: number;
    } | null;
  } | null;
  dsp_tiers?: {
    worker_cpu_s: number;
    audio_s?: number;
    dsp_cpu_per_audio_s?: number | null;
    instructions: number;
    interpreter: number;
    fast_tier: number;
    aot_block: number;
  } | null;
  sink?: {
    stream_started: boolean;
    source_received_frames: number;
    queued_device_frames: number;
    underrun_events: number;
    error: string | null;
    rebuffering?: boolean;
    played_device_frames?: number;
    silent_device_frames?: number;
    stretch_rate?: number;
    recovery?: string;
    sink_capture?: string | null;
    events_total?: number;
  } | null;
}
/** Dev-only inputs of the coupled core: user-supplied, never bundled. */
export interface CoupledFiles {
  syx: Uint8Array;
  image: Uint8Array;
  dsp: Uint8Array;
  snapshot?: Uint8Array;
}
export interface CoupledStats {
  cf_icount?: number;
  frames: number;
  dsp_instructions: number;
  halted: string | null;
  nonzero_replies: number;
  missing_blocks: number;
  pcm_values: number;
  pcm_produced_seconds: number;
  production_elapsed_wall_seconds: number;
  pcm_seconds_per_wall_second: number;
  dsp_attached: boolean;
  pcm_port_connected: boolean;
  host: Record<string, unknown>;
  /** Two-worker session only: the DSP worker's busy time and the ColdFire worker's time blocked on it. */
  split?: { dsp_worker_busy_ms: number; cf_link_wait_ms: number; link_requests: number };
  /** Two-worker session with the fast tier: the kernel host's counters and `digi_sharc_fast_report`. */
  dsp_kernels?: { compiled: number; compileMs: number; wasmBytes: number; runs: number };
  dsp_fast?: unknown;
}
/** Native desktop only: `runtime` patches the digikit runtime into a stock image (the controller's `firmware.load {runtime}`). */
export interface LoadOptions {
  runtime?: boolean;
}
/** The controller's report of the runtime patch of the running image: `patched` with the set and whether the image came from the
 * cache, or `patched: false` for an image that already had a patch record. `null`: stock, no runtime requested. */
export interface RuntimePatch {
  patched: boolean;
  set?: string;
  cache_hit?: boolean;
  modules?: string[];
  seconds?: number;
  reason?: string;
}
export interface FirmwareInfo {
  runtime: RuntimePatch | null;
}
/** The controller's heartbeat (`status.heartbeat`): whether the guest's main loop has stopped, and the stall threshold in emulated ms. */
export interface RuntimeHealth {
  stalled: boolean;
  stall_ms: number;
}
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
  /** Native desktop only: save a report to disk; returns the path written. */
  saveDiagnostics?(name: string, json: string): Promise<string>;
  /** Native desktop only (Digitakt II): the controller builds a card from the files and boots with it. */
  cards?: CardBridge;
  load(bytes: Uint8Array, name: string, options?: LoadOptions): Promise<RuntimeSnapshot>;
  /** Native desktop only: load a firmware file the app can read itself (the startup file), as `load` does. */
  loadPath?(path: string, options?: LoadOptions): Promise<RuntimeSnapshot>;
  /** Native desktop only: what the running image got from the digikit runtime patch (`status.firmware.runtime`). */
  firmwareInfo?(): Promise<FirmwareInfo | undefined>;
  /** Native desktop only: the heartbeat of the running image (`status.heartbeat`), or undefined while a job runs. */
  health?(): Promise<RuntimeHealth | undefined>;
  /** The emulator's own reset of the running firmware (the browser worker boots the file again). Not the device RESTART command. */
  restart(): Promise<RuntimeSnapshot>;
  /** Native desktop only: `dev.reload`. The controller applies the modules again from the objects on disk and brings the running
   * session to them (a module swap into a restored main screen, or a boot of the new image). It never builds. */
  reloadModules?(): Promise<ReloadReport>;
  pause(): void;
  resume(): void;
  press(code: number): Promise<void>;
  release(code: number): Promise<void>;
  turn(encoder: number, detents: number): Promise<void>;
  stop(): Promise<void>;
  dispose(): void;
}
export type RuntimeUpdate = { type: 'snapshot'; snapshot: RuntimeSnapshot } | { type: 'error'; error: string };

export const drawFrame = (canvas: HTMLCanvasElement, frame: Uint8Array) => {
  if (frame.length !== 1024) return;
  const context = canvas.getContext('2d');
  if (!context) return;
  const pixels = context.createImageData(128, 64);
  for (let y = 0; y < 64; y += 1)
    for (let x = 0; x < 128; x += 1) {
      const on = (frame[7 - Math.floor(y / 8) + 8 * x] & (1 << (y % 8))) !== 0;
      const at = (y * 128 + x) * 4;
      pixels.data[at] = on ? 242 : 8;
      pixels.data[at + 1] = on ? 244 : 9;
      pixels.data[at + 2] = on ? 249 : 12;
      pixels.data[at + 3] = 255;
    }
  context.putImageData(pixels, 0, 0);
};

/** Two workers (ColdFire and DSP over a SharedArrayBuffer) need cross-origin isolation; `?split=0` keeps the single worker. */
export const splitAvailable = () =>
  typeof SharedArrayBuffer !== 'undefined' &&
  (globalThis as { crossOriginIsolated?: boolean }).crossOriginIsolated === true &&
  new URLSearchParams(location.search).get('split') !== '0';

export function browserRuntime(onUpdate: (update: RuntimeUpdate) => void): EmulatorRuntime {
  const worker = new Worker(new URL('./runtime-worker.ts', import.meta.url), { type: 'module' });
  let generation = 0,
    paused = true,
    source: Uint8Array | undefined,
    name = '';
  // Two-worker session: the DSP worker, its link and its latest statistics.
  let dsp: Worker | undefined,
    link: SharedArrayBuffer | undefined,
    dspStats:
      | (DspStats & {
          pcm_produced_seconds: number;
          production_elapsed_wall_seconds: number;
          pcm_seconds_per_wall_second: number;
        })
      | undefined;
  let modulePromise: Promise<WebAssembly.Module> | undefined;
  const closeSplit = () => {
    if (link) new LinkServer(link).close();
    dsp?.terminate();
    dsp = undefined;
    link = undefined;
    dspStats = undefined;
  };
  const coreModule = () => (modulePromise ??= WebAssembly.compileStreaming(fetch(`${base}emulator-core-sharc.wasm`)));
  /** Starts the DSP worker; resolves with its link and the compiled core, or undefined when the core cannot be split. */
  const startDsp = async (files: CoupledFiles, audioPort: MessagePort) => {
    const module = await coreModule();
    if (!WebAssembly.Module.exports(module).some((e) => e.name === 'digi_dsp_reply')) return undefined;
    const fast = await fastConfig(files.syx);
    const buffer = createLinkBuffer(),
      thread = new Worker(new URL('../dsp-worker.ts', import.meta.url), { type: 'module' });
    await new Promise<void>((resolve, reject) => {
      thread.onmessage = ({ data }) => {
        if (data.type === 'ready') resolve();
        else if (data.type === 'stats') dspStats = data;
        else if (data.type === 'error') {
          reject(new Error(data.error));
          onUpdate({ type: 'error', error: data.error });
        }
      };
      thread.onerror = (event) => reject(new Error(event.message));
      const start: DspStart = {
        type: 'start',
        module,
        image: files.image,
        state: files.dsp,
        link: buffer,
        fast,
        pcmPort: audioPort,
      };
      thread.postMessage(start, [audioPort]);
    }).catch((error) => {
      thread.terminate();
      throw error;
    });
    dsp = thread;
    link = buffer;
    return { buffer, module };
  };
  worker.onmessage = ({ data }: MessageEvent<RuntimeUpdate & { generation?: number }>) => {
    if (data.generation === generation) onUpdate(data);
  };
  const request = <T>(type: string, payload: Record<string, unknown> = {}) =>
    new Promise<T>((resolve, reject) => {
      const id = crypto.randomUUID();
      const listener = ({ data }: MessageEvent<{ reply?: string; value?: T; error?: string }>) => {
        if (data.reply !== id) return;
        worker.removeEventListener('message', listener);
        if (data.error) reject(new Error(data.error));
        else resolve(data.value as T);
      };
      worker.addEventListener('message', listener);
      worker.postMessage({ type, id, generation, ...payload });
    });
  return {
    diagnostics: () => request<RuntimeDiagnostics>('diagnostics'),
    async loadCoupled(files, audioPort) {
      generation += 1;
      paused = false;
      source = undefined;
      closeSplit();
      const copy = (bytes?: Uint8Array) => bytes?.slice().buffer;
      if (splitAvailable()) {
        const split = await startDsp(files, audioPort);
        if (split)
          return request<RuntimeSnapshot>('load-coupled', {
            bytes: copy(files.syx),
            snapshot: copy(files.snapshot),
            link: split.buffer,
            module: split.module,
          });
      }
      worker.postMessage({ type: 'audio-port', generation, port: audioPort }, [audioPort]);
      return request<RuntimeSnapshot>('load-coupled', {
        bytes: copy(files.syx),
        image: copy(files.image),
        dsp: copy(files.dsp),
        snapshot: copy(files.snapshot),
      });
    },
    async coupledStats() {
      const cf = await request<CoupledStats & { split?: { link_wait_ms: number; link_requests: number } }>(
        'coupled-stats',
      );
      if (!dsp || !cf.split) return cf;
      const d = dspStats,
        engine = (d?.engine ?? {}) as Partial<CoupledStats>;
      return {
        ...cf,
        frames: engine.frames ?? 0,
        dsp_instructions: engine.dsp_instructions ?? 0,
        halted: engine.halted ?? null,
        nonzero_replies: engine.nonzero_replies ?? 0,
        missing_blocks: engine.missing_blocks ?? 0,
        pcm_values: d?.pcmValues ?? 0,
        pcm_produced_seconds: d?.pcm_produced_seconds ?? 0,
        production_elapsed_wall_seconds: d?.production_elapsed_wall_seconds ?? 0,
        pcm_seconds_per_wall_second: d?.pcm_seconds_per_wall_second ?? 0,
        split: {
          dsp_worker_busy_ms: d?.busyMs ?? 0,
          cf_link_wait_ms: cf.split.link_wait_ms,
          link_requests: cf.split.link_requests,
        },
        dsp_kernels: d?.kernels,
        dsp_fast: d?.fast,
      };
    },
    coupledAvailable: () => request<boolean>('coupled-capable'),
    tap: (code, hold) => request<void>('tap', { code, hold }),
    async load(bytes, selectedName) {
      closeSplit();
      generation += 1;
      paused = false;
      source = bytes.slice();
      name = selectedName;
      return request<RuntimeSnapshot>('load', { bytes: bytes.buffer, name: selectedName });
    },
    async restart() {
      if (!source) throw new Error('No firmware selected');
      closeSplit();
      generation += 1;
      paused = false;
      return request<RuntimeSnapshot>('load', { bytes: source.slice().buffer, name });
    },
    pause() {
      paused = true;
      worker.postMessage({ type: 'pause', generation });
    },
    resume() {
      if (!paused) return;
      paused = false;
      worker.postMessage({ type: 'resume', generation });
    },
    press: (code) => request<void>('button', { code, down: true }),
    release: (code) => request<void>('button', { code, down: false }),
    turn: (encoder, detents) => request<void>('turn', { encoder, detents }),
    async stop() {
      closeSplit();
      generation += 1;
      paused = true;
      source = undefined;
      worker.postMessage({ type: 'stop', generation });
    },
    dispose() {
      closeSplit();
      generation += 1;
      paused = true;
      worker.terminate();
    },
  };
}

export async function nativeRuntime(onUpdate: (update: RuntimeUpdate) => void): Promise<EmulatorRuntime | undefined> {
  let api: typeof import('@tauri-apps/api/core');
  try {
    api = await import('@tauri-apps/api/core');
  } catch {
    return undefined;
  }
  if (!api.isTauri()) return undefined;
  const metrics = hostMetrics('native_ipc');
  let generation = 0,
    activeSession: number | undefined,
    loaded = false,
    paused = true,
    disposed = false,
    epoch = 0,
    timer: number | undefined,
    input = Promise.resolve(),
    lifecycle = Promise.resolve();
  const clearPump = () => {
    paused = true;
    epoch += 1;
    metrics.invalidatePumpGap();
    if (timer) window.clearTimeout(timer);
    timer = undefined;
  };
  const fault = (error: unknown) => {
    clearPump();
    loaded = false;
    onUpdate({ type: 'error', error: String(error) });
  };
  // One protocol method of the controller (docs/CONTROL.md). With a session, the controller refuses the call when the webview's
  // session is no longer the current one; `firmware.load` and `restart` start the session they carry.
  const call = <T>(method: string, params: Record<string, unknown> = {}, session?: number) =>
    api.invoke<T>('emu_call', { method, params, session: session ?? null });
  // A new session: stop the pump, run the call that starts it (`firmware.load` or `restart`, with the new token), take the
  // snapshot of what came up, and start pumping it. A newer session wins over an older one that is still starting.
  const newSession = (starts: (token: number) => Promise<void>) => {
    const token = ++generation;
    clearPump();
    loaded = false;
    activeSession = undefined;
    const work = lifecycle
      .catch(() => {})
      .then(async () => {
        metrics.reset();
        await starts(token);
        const snapshot = await api.invoke<RuntimeSnapshot>('emu_startup');
        if (disposed || token !== generation) return snapshot;
        metrics.loaded();
        activeSession = token;
        loaded = true;
        onUpdate({ type: 'snapshot', snapshot });
        start(token);
        return snapshot;
      });
    lifecycle = work.then(
      () => {},
      () => {},
    );
    return work.catch((error) => {
      if (token === generation) fault(error);
      throw error;
    });
  };
  const pump = async (token: number, run: number) => {
    if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
    await input.catch(() => {});
    if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
    try {
      const begin = performance.now();
      const snapshot = await api.invoke<RuntimeSnapshot>('emu_step', { sessionId: token, budget: 250_000 });
      if (disposed || paused || token !== generation || run !== epoch || activeSession !== token) return;
      if (!snapshot.polled) metrics.step(begin, performance.now(), snapshot);
      onUpdate({ type: 'snapshot', snapshot });
      if (snapshot.status.error) {
        fault(snapshot.status.error);
        return;
      }
      // The controller steps the guest itself: poll once per display frame (a stale callback returns on the epoch check).
      if (snapshot.polled)
        window.requestAnimationFrame(() => {
          void pump(token, run);
        });
      else
        timer = window.setTimeout(() => {
          void pump(token, run);
        }, 0);
    } catch (error) {
      if (token === generation && run === epoch) fault(error);
    }
  };
  const start = (token: number) => {
    metrics.invalidatePumpGap();
    paused = false;
    const run = ++epoch;
    timer = window.setTimeout(() => {
      void pump(token, run);
    }, 0);
  };
  const queueInput = (operation: (session: number) => Promise<void>) => {
    const token = generation,
      session = activeSession;
    metrics.invalidatePumpGap();
    const next = input
      .catch(() => {})
      .then(async () => {
        if (!disposed && loaded && token === generation && session === activeSession && session !== undefined)
          await operation(session);
      });
    input = next.then(
      () => {
        if (!disposed && token === generation && session === activeSession) metrics.invalidatePumpGap();
      },
      (error) => {
        if (!disposed && token === generation && session === activeSession) metrics.invalidatePumpGap();
        throw error;
      },
    );
    return input.catch((error) => {
      if (token === generation) fault(error);
      throw error;
    });
  };
  try {
    metrics.reset();
    const startup = await api.invoke<RuntimeSnapshot>('emu_startup');
    metrics.loaded();
    generation = 1;
    activeSession = 1;
    loaded = true;
    onUpdate({ type: 'snapshot', snapshot: startup });
    start(1);
  } catch (error) {
    if (!String(error).includes('No startup firmware selected')) onUpdate({ type: 'error', error: String(error) });
  }
  return {
    saveDiagnostics: (name: string, json: string) => api.invoke<string>('emu_save_diagnostics', { name, json }),
    cards: {
      reset: () => api.invoke<void>('emu_card_reset'),
      stage: (name, bytes) =>
        api.invoke<void>('emu_card_stage', bytes, { headers: { 'x-name': encodeURIComponent(name) } }),
      load: (name) => api.invoke<unknown>('emu_card_load', { name: name ?? null }),
      status: () => call<CardStatus>('card.status'),
    },
    async diagnostics() {
      const session = activeSession;
      if (session === undefined) throw new Error('No firmware selected');
      const report = await call<RuntimeDiagnostics>('diagnostics', {}, session);
      if (disposed || activeSession !== session) throw new Error('Firmware session changed during diagnostic export');
      return { ...report, host: metrics.report() };
    },
    async nativeAudio() {
      const session = activeSession;
      if (session === undefined) return undefined;
      const report = await api.invoke<NativeAudioStatus | null>('emu_audio_status', { sessionId: session });
      if (disposed || activeSession !== session) throw new Error('Firmware session changed while reading audio status');
      return (
        report ?? {
          sink_connected: false,
          playback_requested: false,
          flow_started: false,
          source_audio_seconds: 0,
          session_wall_seconds: 0,
          device_error: null,
          setup_needed: true,
        }
      );
    },
    // Open...: the app puts the picked bytes in a file, then `firmware.load` reads it like any other.
    load: (bytes, name, options) =>
      newSession(async (token) => {
        const syx = await api.invoke<string>('emu_stage_syx', bytes, {
          headers: { 'x-name': encodeURIComponent(name) },
        });
        await call('firmware.load', { syx, replace: true, runtime: !!options?.runtime }, token);
      }),
    loadPath: (syx, options) =>
      newSession(async (token) => {
        await call('firmware.load', { syx, replace: true, runtime: !!options?.runtime }, token);
      }),
    async firmwareInfo() {
      const status = await call<{ firmware: { runtime?: RuntimePatch | null } | null }>('status');
      return { runtime: status.firmware?.runtime ?? null };
    },
    async health() {
      const status = await call<{ heartbeat?: RuntimeHealth }>('status');
      return status.heartbeat;
    },
    async restart() {
      if (!loaded || activeSession === undefined) throw new Error('No firmware selected');
      return newSession(async (token) => {
        await call('restart', {}, token);
      });
    },
    // The session stays the same (the call carries its token); the pump stops while the controller loads or restores, a cold boot
    // is a job that refuses steps.
    async reloadModules() {
      const session = activeSession;
      if (!loaded || session === undefined) throw new Error('No firmware selected');
      const resume = !paused;
      clearPump();
      const work = lifecycle
        .catch(() => {})
        .then(async () => {
          try {
            const report = await call<ReloadReport>('dev.reload', { build: true }, session);
            if (disposed || activeSession !== session) return report;
            onUpdate({ type: 'snapshot', snapshot: await api.invoke<RuntimeSnapshot>('emu_startup') });
            return report;
          } finally {
            if (!disposed && activeSession === session && resume) start(session);
          }
        });
      lifecycle = work.then(
        () => {},
        () => {},
      );
      return work;
    },
    pause() {
      clearPump();
    },
    resume() {
      if (loaded && activeSession !== undefined && paused) start(activeSession);
    },
    press: (code) => queueInput((session) => call<void>('input.button', { code, down: true }, session)),
    release: (code) => queueInput((session) => call<void>('input.button', { code, down: false }, session)),
    turn: (encoder, detents) => queueInput((session) => call<void>('input.turn', { encoder, detents }, session)),
    tap: (code, hold) => {
      const token = generation;
      return queueInput(async (session) => {
        const snapshot = await api.invoke<RuntimeSnapshot>('emu_tap', { sessionId: session, code, hold });
        if (!disposed && token === generation && activeSession === session) onUpdate({ type: 'snapshot', snapshot });
      });
    },
    async stop() {
      const token = ++generation;
      clearPump();
      loaded = false;
      activeSession = undefined;
      await (lifecycle = lifecycle.catch(() => {}).then(() => api.invoke<void>('emu_stop', { sessionId: token })));
    },
    dispose() {
      disposed = true;
      const token = ++generation;
      clearPump();
      loaded = false;
      activeSession = undefined;
      void api.invoke<void>('emu_stop', { sessionId: token });
    },
  };
}
