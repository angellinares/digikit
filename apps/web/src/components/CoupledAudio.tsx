import { Show, createEffect, createSignal, onCleanup, onMount } from 'solid-js';
import { PcmSink, resumeOnNextGesture } from '../runtime/audio';
import { fetchLocalInputs, localManifest } from '../runtime/local-inputs';
import type { EmulatorRuntime, RuntimeSnapshot } from '../runtime/runtime';

/** Load logic shared by the panel's Start button and Faceplate's local-inputs path. The caller creates and resumes `sink` in its gesture and awaits that before calling; throws on failure and leaves closing `sink` to the caller. */
export async function startCoupled(opts: {
  runtime: EmulatorRuntime;
  firmware: { name: string; bytes: Uint8Array };
  image: Uint8Array;
  dsp: Uint8Array;
  snapshot?: Uint8Array;
  sink: PcmSink;
  bufferSeconds?: number;
  onStarted?: (snapshot: RuntimeSnapshot) => void;
}): Promise<RuntimeSnapshot> {
  const { runtime, sink } = opts;
  if (!runtime.loadCoupled || !(await runtime.coupledAvailable?.()))
    throw new Error('Audio setup needs a browser core built with SHARC support.');
  const port = await sink.open(opts.bufferSeconds ?? 0);
  const snapshot = await runtime.loadCoupled(
    { syx: opts.firmware.bytes.slice(), image: opts.image, dsp: opts.dsp, snapshot: opts.snapshot },
    port,
  );
  opts.onStarted?.(snapshot);
  return snapshot;
}

/**
 * Browser setup for coupled ColdFire + SHARC+ audio. Every file is
 * user-supplied and stays in the page: firmware (.syx), the packed DSP
 * image, the DSP state (`.dsp` beside a coupled snapshot) and, to skip the
 * boot, the ColdFire snapshot taken at ready. Needs a core built with
 * `node tools/nx/wasm.mjs --sharc`.
 */
export default function CoupledAudio(props: {
  runtime: () => Promise<EmulatorRuntime | undefined>;
  reset: number;
  firmware?: () => { name: string; bytes: Uint8Array } | undefined;
  adopted?: () => { sink: PcmSink; note: string } | undefined;
  onStarted: (snapshot: RuntimeSnapshot) => void;
  onError: (error: string) => void;
}) {
  const files: Record<'syx' | 'image' | 'dsp' | 'snapshot', File | undefined> = {
    syx: undefined,
    image: undefined,
    dsp: undefined,
    snapshot: undefined,
  };
  const [buffer, setBuffer] = createSignal(2);
  const [report, setReport] = createSignal('');
  const [active, setActive] = createSignal(false);
  const [available, setAvailable] = createSignal(false);
  const [localFound, setLocalFound] = createSignal(false);
  let sink: PcmSink | undefined;
  let firmwareNote = '';
  let timer: number | undefined;
  let runtime: EmulatorRuntime | undefined;
  const read = async (file: File) => new Uint8Array(await file.arrayBuffer());
  const poll = async () => {
    try {
      const stats = await runtime?.coupledStats?.();
      const a = sink?.stats;
      if (stats && a)
        setReport(
          `${firmwareNote} · DSP frames ${stats.frames} · produced ${stats.pcm_produced_seconds.toFixed(2)} s · ${stats.pcm_seconds_per_wall_second.toFixed(2)} audio s/wall s · queued ${a.queuedSeconds.toFixed(2)} s (peak ${a.highWaterSeconds.toFixed(2)}) · ${a.playing ? 'playing' : 'buffering'} · underruns ${a.underruns} (${a.underrunSeconds.toFixed(2)} s)${sink && sink.context.state !== 'running' ? ' · audio suspended — click anywhere to start' : ''}${stats.halted ? ` · DSP HALTED: ${stats.halted}` : ''}`,
        );
    } catch {
      /* session replaced */
    }
  };
  const exportHealth = async () => {
    try {
      if (!runtime?.coupledStats || !sink) return;
      const report = { schema_version: 1, core: await runtime.coupledStats(), sink: sink.report() };
      const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = 'audio-health.json';
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (error) {
      props.onError(String(error));
    }
  };
  const start = async () => {
    // Create and resume the AudioContext before any await, inside the click gesture.
    const fresh = new PcmSink();
    const resumed = fresh.resume();
    void resumed.catch(() => {});
    resumeOnNextGesture(fresh);
    setActive(false);
    window.clearInterval(timer);
    sink?.close();
    sink = fresh;
    try {
      const top = files.syx ? undefined : props.firmware?.();
      if (!files.syx && !top) {
        sink.close();
        sink = undefined;
        props.onError('Choose the firmware (top or here), the DSP image and the DSP state first.');
        return;
      }
      // Dev server only: fill missing DSP image/state (and snapshot) from the local inputs.
      const local = !files.image || !files.dsp ? await fetchLocalInputs() : undefined;
      if ((!files.image && !local) || (!files.dsp && !local)) {
        sink.close();
        sink = undefined;
        props.onError('Choose the firmware (top or here), the DSP image and the DSP state first.');
        return;
      }
      firmwareNote = files.syx ? `firmware: ${files.syx.name}` : `firmware: ${top!.name} (from Choose firmware)`;
      runtime = await props.runtime();
      if (!runtime?.loadCoupled || !(await runtime.coupledAvailable?.())) {
        sink.close();
        sink = undefined;
        setAvailable(false);
        setReport('Audio setup needs a browser core built with SHARC support.');
        return;
      }
      setReport(`Loading… ${firmwareNote}`);
      const [syx, image, dsp, picked] = await Promise.all([
        files.syx ? read(files.syx) : Promise.resolve(top!.bytes.slice()),
        files.image ? read(files.image) : Promise.resolve(local!.image),
        files.dsp ? read(files.dsp) : Promise.resolve(local!.state),
        files.snapshot ? read(files.snapshot) : Promise.resolve(undefined),
      ]);
      const snapshot = picked ?? local?.snapshot;
      props.onStarted(
        await startCoupled({
          runtime,
          firmware: { name: files.syx?.name ?? top!.name, bytes: syx },
          image,
          dsp,
          snapshot,
          sink,
          bufferSeconds: buffer(),
        }),
      );
      setActive(true);
      timer = window.setInterval(() => {
        void poll();
      }, 500);
    } catch (error) {
      setActive(false);
      window.clearInterval(timer);
      sink?.close();
      sink = undefined;
      props.onError(String(error));
      setReport(String(error));
    }
  };
  onCleanup(() => {
    window.clearInterval(timer);
    sink?.close();
  });
  // A session started programmatically (Faceplate) hands over its sink so stats and export work.
  createEffect(() => {
    const adopted = props.adopted?.();
    if (!adopted) return;
    window.clearInterval(timer);
    sink?.close();
    sink = adopted.sink;
    firmwareNote = adopted.note;
    void props.runtime().then((next) => {
      runtime = next;
    });
    setActive(true);
    timer = window.setInterval(() => {
      void poll();
    }, 500);
  });
  onMount(() => {
    void localManifest().then((m) => setLocalFound(Boolean(m?.dspImage && m.dspState)));
  });
  createEffect(() => {
    void props.reset;
    setActive(false);
    window.clearInterval(timer);
    sink?.close();
    sink = undefined;
  });
  onMount(() => {
    void props.runtime().then(async (next) => {
      runtime = next;
      const capable = Boolean(next?.loadCoupled && (await next.coupledAvailable?.()));
      setAvailable(capable);
      if (!capable) setReport('Audio setup needs a browser core built with SHARC support.');
    });
  });
  const pick = (key: keyof typeof files) => (e: Event & { currentTarget: HTMLInputElement }) => {
    files[key] = e.currentTarget.files?.[0];
  };
  return (
    <div
      class="coupled"
      style={{ display: 'flex', 'flex-wrap': 'wrap', gap: '0.5rem', 'align-items': 'center', padding: '0.4rem 0' }}
    >
      <Show when={available()} fallback={<output>{report() || 'Checking browser audio support…'}</output>}>
        <Show when={localFound()}>
          <small>Local DSP inputs found (dev server)</small>
        </Show>
        <label>
          Firmware .syx <input type="file" accept=".syx" onChange={pick('syx')} />
        </label>
        <label>
          DSP image <input type="file" onChange={pick('image')} />
        </label>
        <label>
          DSP state <input type="file" onChange={pick('dsp')} />
        </label>
        <label>
          CF snapshot (optional) <input type="file" onChange={pick('snapshot')} />
        </label>
        <label>
          Buffer s{' '}
          <input
            type="number"
            min="0"
            step="0.5"
            value={buffer()}
            style={{ width: '4rem' }}
            onInput={(e) => {
              const v = Number(e.currentTarget.value);
              setBuffer(v);
              sink?.setBuffer(v);
            }}
          />
        </label>
        <button onClick={() => void start()}>Start emulator audio</button>
        <button disabled={!active()} onClick={() => void runtime?.tap?.(25).catch((e) => props.onError(String(e)))}>
          Trig 1
        </button>
        <small>From the ready snapshot, press NO once before Trig 1.</small>
        <button disabled={!active()} onClick={() => void exportHealth()}>
          Export audio health
        </button>
        <output>{report()}</output>
        <small style={{ width: '100%' }}>
          The browser core runs well below real time: audio plays in bursts. Buffer s sets how much is queued before
          each burst.
        </small>
      </Show>
    </div>
  );
}
