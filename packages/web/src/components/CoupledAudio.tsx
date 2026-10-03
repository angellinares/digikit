import { createSignal, onCleanup } from 'solid-js';
import { PcmSink } from '../audio';
import type { EmulatorRuntime, RuntimeSnapshot } from '../runtime';

/**
 * Dev/debug row for coupled ColdFire + SHARC+ audio (`?audio=1`). Every file
 * is user-supplied and stays in the page: firmware (.syx), the packed DSP
 * image, the DSP state (`.dsp` beside a coupled snapshot) and, to skip the
 * boot, the ColdFire snapshot taken at ready. Needs a core built with
 * `tools/native_wasm.sh --sharc`.
 */
export default function CoupledAudio(props: { runtime: () => Promise<EmulatorRuntime | undefined>; onStarted: (snapshot: RuntimeSnapshot) => void; onError: (error: string) => void }) {
  const files: Record<'syx' | 'image' | 'dsp' | 'snapshot', File | undefined> = { syx: undefined, image: undefined, dsp: undefined, snapshot: undefined };
  const [buffer, setBuffer] = createSignal(2);
  const [report, setReport] = createSignal('');
  const [active, setActive] = createSignal(false);
  let sink: PcmSink | undefined;
  let timer: number | undefined;
  let runtime: EmulatorRuntime | undefined;
  const read = async (file: File) => new Uint8Array(await file.arrayBuffer());
  const poll = async () => {
    try {
      const stats = await runtime?.coupledStats?.();
      const a = sink?.stats;
      if (stats && a) setReport(`DSP frames ${stats.frames} · produced ${stats.pcm_produced_seconds.toFixed(2)} s · ${stats.pcm_seconds_per_wall_second.toFixed(2)} audio s/wall s · queued ${a.queuedSeconds.toFixed(2)} s (peak ${a.highWaterSeconds.toFixed(2)}) · ${a.playing ? 'playing' : 'buffering'} · underruns ${a.underruns} (${a.underrunSeconds.toFixed(2)} s)${stats.halted ? ` · DSP HALTED: ${stats.halted}` : ''}`);
    } catch { /* session replaced */ }
  };
  const exportHealth = async () => {
    try {
      if (!runtime?.coupledStats || !sink) return;
      const report = { schema_version: 1, core: await runtime.coupledStats(), sink: sink.report() };
      const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a'); link.href = url; link.download = 'audio-health.json'; link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (error) { props.onError(String(error)); }
  };
  const start = async () => {
    try {
      if (!files.syx || !files.image || !files.dsp) { props.onError('Choose the firmware, DSP image and DSP state first.'); return; }
      setReport('Loading…'); sink?.close(); window.clearInterval(timer);
      sink = new PcmSink(); const port = await sink.open(buffer());
      runtime = await props.runtime();
      if (!runtime?.loadCoupled) { props.onError('This runtime has no coupled-audio path (browser worker with a sharc core is required).'); return; }
      const [syx, image, dsp] = await Promise.all([read(files.syx), read(files.image), read(files.dsp)]);
      const snapshot = files.snapshot ? await read(files.snapshot) : undefined;
      props.onStarted(await runtime.loadCoupled({ syx, image, dsp, snapshot }, port));
      setActive(true); timer = window.setInterval(() => { void poll(); }, 500);
    } catch (error) { props.onError(String(error)); setReport(String(error)); }
  };
  onCleanup(() => { window.clearInterval(timer); sink?.close(); });
  const pick = (key: keyof typeof files) => (e: Event & { currentTarget: HTMLInputElement }) => { files[key] = e.currentTarget.files?.[0]; };
  return <div class="coupled" style={{ display: 'flex', 'flex-wrap': 'wrap', gap: '0.5rem', 'align-items': 'center', padding: '0.4rem 0' }}>
    <label>Firmware .syx <input type="file" accept=".syx" onChange={pick('syx')} /></label>
    <label>DSP image <input type="file" onChange={pick('image')} /></label>
    <label>DSP state <input type="file" onChange={pick('dsp')} /></label>
    <label>CF snapshot (optional) <input type="file" onChange={pick('snapshot')} /></label>
    <label>Buffer s <input type="number" min="0" step="0.5" value={buffer()} style={{ width: '4rem' }} onInput={(e) => { const v = Number(e.currentTarget.value); setBuffer(v); sink?.setBuffer(v); }} /></label>
    <button onClick={() => void start()}>Start coupled audio</button>
    <button disabled={!active()} onClick={() => void runtime?.tap?.(25).catch((e) => props.onError(String(e)))}>Trig 1</button>
    <button disabled={!active()} onClick={() => void exportHealth()}>Export audio health</button>
    <output>{report()}</output>
  </div>;
}
