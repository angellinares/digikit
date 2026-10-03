import { For, Show, createSignal, onCleanup, onMount } from 'solid-js';
import { panels } from '../data/faceplates';
import { browserRuntime, drawFrame, nativeRuntime, type EmulatorRuntime, type NativeAudioStatus, type RuntimeUpdate } from '../runtime';
import CoupledAudio from './CoupledAudio';
import './faceplate.css';

const iconPaths: Record<string, string> = {
  perform: 'M8 17.5 V4 Q10 6 11 7 M13 5 H21 M13 10 H21 M13 15 H21 M13 20 H21',
  settings: 'M12 2.5 L14.2 5.3 L17.7 4.3 L18.1 7.9 L21.5 9.2 L19.6 12 L21.5 14.8 L18.1 16.1 L17.7 19.7 L14.2 18.7 L12 21.5 L9.8 18.7 L6.3 19.7 L5.9 16.1 L2.5 14.8 L4.4 12 L2.5 9.2 L5.9 7.9 L6.3 4.3 L9.8 5.3 Z',
  samples: 'M3 10 V14 M6.6 7 V17 M10.2 4 V20 M13.8 8.5 V15.5 M17.4 5.5 V18.5 M21 3 V21',
  tempo: 'M5 21 L9.5 3 H14.5 L19 21 Z M11 16 L20 6', keyboard: 'M5 3 H19 Q21 3 21 5 V19 Q21 21 19 21 H5 Q3 21 3 19 V5 Q3 3 5 3 Z M7.5 14 V21 M12 14 V21 M16.5 14 V21',
  record: 'M12 4 A8 8 0 1 0 12 20 A8 8 0 0 0 12 4', play: 'M7 4 L19.5 12 L7 20 Z', stop: 'M8 4.5 H16 Q19.5 4.5 19.5 8 V16 Q19.5 19.5 16 19.5 H8 Q4.5 19.5 4.5 16 V8 Q4.5 4.5 8 4.5 Z',
  up: 'M6 18.5 L12 5 L18 18.5', down: 'M6 5.5 L12 19 L18 5.5', left: 'M17 5 L6.5 12 L17 19', right: 'M7 5 L17.5 12 L7 19',
  unison: 'M6.3 7 L11 16.5 M12 6.2 V16.5 M17.7 7 L13 16.5', arp: 'M8.4 18.5 V7 M18.4 16.5 V5', noteedit: 'M14 3.5 V18', plus: 'M12 5 V19 M5 12 H19', minus: 'M5 12 H19', headphones: 'M4.5 15 V12 C4.5 2 19.5 2 19.5 12 V15',
};
function Icon(props: { name: string }) { return <g class="icon" aria-hidden="true"><path class="icon-stroke" d={iconPaths[props.name] ?? ''} /><Show when={props.name === 'settings'}><circle class="icon-stroke" cx="12" cy="12" r="3" /></Show><Show when={props.name === 'perform'}><circle class="icon-fill" cx="5.5" cy="17.5" r="2.6" /></Show><Show when={props.name === 'arp'}><circle class="icon-fill" cx="6" cy="18.5" r="2.6"/><circle class="icon-fill" cx="16" cy="16.5" r="2.6"/><path class="icon-fill" d="M8.4 5.8 L18.4 3.8 V7 L8.4 9 Z" /></Show><Show when={props.name === 'noteedit'}><circle class="icon-fill" cx="11.8" cy="19" r="2.5"/><circle class="icon-fill" cx="11.8" cy="13" r="1.5"/><circle class="icon-fill" cx="11.8" cy="9" r="1.5"/><circle class="icon-fill" cx="11.8" cy="5" r="1.5"/></Show><Show when={props.name === 'unison'}><circle class="icon-stroke" cx="5" cy="5" r="2.2"/><circle class="icon-stroke" cx="12" cy="4" r="2.2"/><circle class="icon-stroke" cx="19" cy="5" r="2.2"/><circle class="icon-fill" cx="12" cy="19" r="2.6"/></Show><Show when={props.name === 'keyboard'}><rect class="icon-fill" x="6" y="3" width="3" height="11"/><rect class="icon-fill" x="10.5" y="3" width="3" height="11"/><rect class="icon-fill" x="15" y="3" width="3" height="11"/></Show><Show when={props.name === 'headphones'}><rect class="icon-fill" x="3" y="13.5" width="4.5" height="7.5"/><rect class="icon-fill" x="16.5" y="13.5" width="4.5" height="7.5"/></Show></g>; }

export default function Faceplate(props: { runtime?: EmulatorRuntime } = {}) {
  const [device, setDevice] = createSignal<'dt2' | 'dn2'>('dt2');
  const [pressed, setPressed] = createSignal(new Set<number>());
  const [status, setStatus] = createSignal('Choose a firmware image to start the diagnostic runtime.');
  const [reportAvailable, setReportAvailable] = createSignal(false);
  const [paused, setPaused] = createSignal(true);
  const [loaded, setLoaded] = createSignal(false);
  const [browserAudioSetup, setBrowserAudioSetup] = createSignal(false);
  const [browserAudioReset, setBrowserAudioReset] = createSignal(0);
  const [nativeAudio, setNativeAudio] = createSignal<NativeAudioStatus>();
  const runtime = () => activeRuntime ?? props.runtime;
  let activeRuntime: EmulatorRuntime | undefined;
  let runtimeReady: Promise<EmulatorRuntime | undefined> | undefined;
  let canvas: HTMLCanvasElement | undefined;
  const pointers = new Map<number, { code?: number; encoder?: number; y: number; pushed?: number; moved: boolean; target?: SVGGElement }>();
  const owners = new Map<number, number>();
  let suppressedKnob: SVGGElement | undefined;
  let clearSuppression: number | undefined;
  let audioTimer: number | undefined;
  let firmwareFile: File | undefined;
  const panel = () => panels[device()];
  const clearCanvas = () => { if (canvas) canvas.getContext('2d')?.clearRect(0, 0, 128, 64); };
  const update = (event: RuntimeUpdate) => { if (event.type === 'error') { setPaused(true); setLoaded(false); setStatus(event.error); return; } const snapshot = event.snapshot; setReportAvailable(true); if (snapshot.frame && canvas) drawFrame(canvas, snapshot.frame); const s = snapshot.status; if (s.error) { setPaused(true); setLoaded(false); } else if (!loaded()) { setLoaded(true); setPaused(false); } if (s.device === 'dt2' || s.device === 'dn2') setDevice(s.device); const display = s.frame_revision === 0 ? 'Booting — waiting for display' : s.frame_source === 'intro' ? 'Booting — startup display' : s.phase; setStatus(`${s.device} ${s.version} — ${display}, ${s.icount.toLocaleString()} instructions${s.error ? `: ${s.error}` : ''}`); };
  const emit = (code: number, down: boolean) => { const count = owners.get(code) ?? 0; const nextCount = down ? count + 1 : Math.max(0, count - 1); if (nextCount) owners.set(code, nextCount); else owners.delete(code); if ((down && count === 0) || (!down && count === 1)) { setPressed((old) => { const next = new Set(old); down ? next.add(code) : next.delete(code); return next; }); const result = down ? runtime()?.press(code) : runtime()?.release(code); void result?.catch((error) => update({ type: 'error', error: String(error) })); } };
  const releasePointer = (id: number) => { const state = pointers.get(id); if (!state) return; if (state.target && (state.moved || state.pushed)) { suppressedKnob = state.target; if (clearSuppression) window.clearTimeout(clearSuppression); clearSuppression = window.setTimeout(() => { suppressedKnob = undefined; }, 0); } if (state.code) emit(state.code, false); if (state.pushed) emit(state.pushed, false); pointers.delete(id); };
  const releaseAll = () => { for (const code of pressed()) runtime()?.release(code); owners.clear(); setPressed(new Set<number>()); pointers.clear(); suppressedKnob = undefined; };
  onMount(() => window.addEventListener('blur', releaseAll));
  let disposed = false;
  const refreshNativeAudio = async () => {
    try { setNativeAudio(await activeRuntime?.nativeAudio?.()); } catch { /* a replaced session has no status to show */ }
  };
  onMount(() => { runtimeReady = Promise.resolve(props.runtime ?? nativeRuntime(update)).then((runtime) => { const selected = runtime ?? browserRuntime(update); if (disposed) { selected.dispose(); return undefined; } activeRuntime = selected; setBrowserAudioSetup(!runtime); void refreshNativeAudio(); audioTimer = window.setInterval(() => { void refreshNativeAudio(); }, 1000); return selected; }); });
  onCleanup(() => { disposed = true; window.removeEventListener('blur', releaseAll); if (clearSuppression) window.clearTimeout(clearSuppression); if (audioTimer) window.clearInterval(audioTimer); releaseAll(); void activeRuntime?.stop().catch((error) => update({ type: 'error', error: String(error) })); activeRuntime?.dispose(); });
  const choose = async (file?: File) => {
    if (!file) return;
    releaseAll(); setBrowserAudioReset((value) => value + 1); void runtime()?.stop(); clearCanvas(); setReportAvailable(false); setLoaded(false); firmwareFile = file; setStatus(`Loading ${file.name}…`);
    try { const bytes = new Uint8Array(await file.arrayBuffer()); if (firmwareFile !== file) return; const selected = await runtimeReady; if (firmwareFile !== file || !selected) return; const snapshot = await selected.load(bytes, file.name); if (firmwareFile !== file) return; update({ type: 'snapshot', snapshot }); setPaused(false); setLoaded(true); } catch (error) { if (firmwareFile === file) { setPaused(true); setLoaded(false); setStatus(String(error)); } }
  };
  const exportDiagnostics = async () => {
    try {
      const report = await runtime()?.diagnostics();
      if (!report) return;
      const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a'); link.href = url; link.download = `digiemu-${report.device}-diagnostics.json`; link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (error) { setStatus(`Diagnostic export failed: ${String(error)}`); }
  };
  const restart = async () => { try { const snapshot = await runtime()?.restart(); if (snapshot) update({ type: 'snapshot', snapshot }); setPaused(false); } catch (error) { setPaused(true); setLoaded(false); setStatus(String(error)); } };
  return <main style={{ '--accent': panel().accent, '--func-legend': panel().func_legend }}>
    <header class="toolbar"><label class="file">Choose firmware<input type="file" accept=".syx,application/octet-stream" onChange={(e) => choose(e.currentTarget.files?.[0])} /></label><select aria-label="Device" value={device()} onChange={(e) => { releaseAll(); setBrowserAudioReset((value) => value + 1); void runtime()?.stop(); clearCanvas(); firmwareFile = undefined; const input = document.querySelector<HTMLInputElement>('.file input'); if (input) input.value = ''; setReportAvailable(false); setDevice(e.currentTarget.value as 'dt2' | 'dn2'); setPaused(true); setLoaded(false); setStatus('Choose a firmware image to start.'); }}><option value="dt2">Digitakt II</option><option value="dn2">Digitone II</option></select><button disabled={!loaded()} onClick={() => { if (paused()) { runtime()?.resume(); setPaused(false); } else { runtime()?.pause(); setPaused(true); } }}>{paused() ? 'Resume' : 'Pause'}</button><button disabled={!loaded()} onClick={() => void restart()}>Restart</button><button disabled={!reportAvailable()} onClick={() => void exportDiagnostics()}>Export diagnostics</button><output>{status()}</output></header>
    <Show when={browserAudioSetup()}><CoupledAudio reset={browserAudioReset()} runtime={() => runtimeReady ?? Promise.resolve(undefined)} onStarted={(snapshot) => { releaseAll(); clearCanvas(); update({ type: 'snapshot', snapshot }); setPaused(false); setLoaded(true); }} onError={(error) => setStatus(error)} /></Show>
    <Show when={nativeAudio()}>{(audio) => <div class="coupled" style={{ display: 'flex', 'flex-wrap': 'wrap', gap: '0.5rem', 'align-items': 'center', padding: '0.4rem 0' }}><output>{audio().setup_needed ? 'Audio setup is unavailable for this firmware.' : !audio().playback_requested ? 'Audio output muted.' : audio().device_error ?? audio().sink?.error ?? (audio().sink_connected ? (audio().sink?.stream_started ? `Audio output active${audio().output_device ? ` on ${audio().output_device}` : ''} · underruns ${audio().sink?.underrun_events ?? 0} · produced ${audio().source_audio_seconds.toFixed(2)} s` : 'Audio output ready; buffering.') : 'Audio output is unavailable.')}</output><button disabled={!loaded() || audio().setup_needed || !audio().flow_started} onClick={() => void runtime()?.tap?.(25).catch((error) => update({ type: 'error', error: String(error) }))}>Audition Trig 1</button></div>}</Show>
    <section class="panel-wrap" aria-label={`${panel().name} front panel`}><div class="panel-surface"><svg class="panel" viewBox="0 0 215 176" onPointerMove={(e) => { const drag = pointers.get(e.pointerId); if (!drag?.encoder) return; const detents = Math.trunc((drag.y - e.clientY) / 8); if (Math.abs(e.clientY - drag.y) > 3) drag.moved = true; if (detents) { runtime()?.turn(drag.encoder, detents); drag.y -= detents * 8; } }} onPointerUp={(e) => releasePointer(e.pointerId)} onPointerCancel={(e) => releasePointer(e.pointerId)} onLostPointerCapture={(e) => releasePointer(e.pointerId)}>
      <defs><linearGradient id="chassis" x2="0" y2="1"><stop stop-color="#34363a"/><stop offset=".5" stop-color="#2c2e31"/><stop offset="1" stop-color="#242528"/></linearGradient><radialGradient id="knob"><stop stop-color="#44464b"/><stop offset=".68" stop-color="#2a2c2f"/><stop offset=".72" stop-color="#111214"/></radialGradient></defs>
      <rect class="chassis" x="1" y="1" width="213" height="174" rx="3"/><For each={panel().screws}>{(s) => <g class="screw"><circle cx={s.x} cy={s.y} r="2"/><path d={`M${s.x-1.1} ${s.y}h2.2`}/></g>}</For>
      <For each={panel().leds.y}>{(y) => <For each={panel().leds.x}>{(x) => <circle class="led" cx={x} cy={y} r={panel().leds.r}/>}</For>}</For>
      <For each={panel().texts}>{(t) => t.style === 'icon' ? <g class="panel-icon" transform={`translate(${t.x-1.7} ${t.y-1.7}) scale(.142)`}><Icon name="headphones"/></g> : <text class={`silk ${t.style}`} x={t.x} y={t.y}>{t.text}</text>}</For>
      <rect class="bezel" x={panel().bezel[0]} y={panel().bezel[1]} width={panel().bezel[2]-panel().bezel[0]} height={panel().bezel[3]-panel().bezel[1]} rx="1.5"/><rect class="glass" x={panel().glass[0]} y={panel().glass[1]} width={panel().glass[2]-panel().glass[0]} height={panel().glass[3]-panel().glass[1]} rx=".5"/>
      <text class="wordmark" x={panel().wordmark[0]} y={panel().wordmark[1]}>Digiemu</text>
      <For each={panel().knobs}>{(knob) => <g class="knob" onPointerDown={(e) => { if (!knob.encoder) return; const pushed = e.shiftKey ? knob.push : undefined; if (pushed) emit(pushed, true); pointers.set(e.pointerId, { encoder: knob.encoder, y: e.clientY, pushed, moved: false, target: e.currentTarget }); e.currentTarget.setPointerCapture(e.pointerId); }} onClick={(e) => { if (suppressedKnob === e.currentTarget) { suppressedKnob = undefined; return; } if (knob.push) { emit(knob.push, true); queueMicrotask(() => emit(knob.push!, false)); } }} onWheel={(e) => { if (!knob.encoder) return; e.preventDefault(); runtime()?.turn(knob.encoder, e.shiftKey ? Math.sign(-e.deltaY) * 10 : Math.sign(-e.deltaY)); }}><circle cx={knob.x+.65} cy={knob.y+1} r={knob.r} class="knob-shadow"/><circle cx={knob.x} cy={knob.y} r={knob.r} class="knob-face"/><circle cx={knob.x} cy={knob.y} r={knob.r*.84} class="knob-top"/><Show when={knob.marker === 'dot'}><circle class="knob-dot" cx={knob.x+knob.r*.58} cy={knob.y} r={knob.r*.12}/></Show><Show when={knob.marker !== 'dot'}><path d={`M${knob.x} ${knob.y-knob.r*.78}v${knob.r*.33}`} /></Show><text x={knob.x} y={knob.label_y ?? knob.y + knob.r + 3.1}>{knob.label}</text><Show when={knob.sub}><text class="knob-sub" x={knob.x} y={(knob.label_y ?? knob.y + knob.r + 3.1) + 2.4}>{knob.sub}</text></Show></g>}</For>
      <For each={panel().keys}>{(key) => <g class={`key ${key.style ?? ''} ${key.icon ? `icon-${key.icon}` : ''} ${pressed().has(key.code) ? 'down' : ''}`} onPointerDown={(e) => { e.currentTarget.setPointerCapture(e.pointerId); pointers.set(e.pointerId, { code: key.code, y: e.clientY, moved: false }); emit(key.code, true); }}><rect x={key.x-key.w/2+.8} y={key.y-key.h/2+1.2} width={key.w-1.6} height={key.h-1.2} rx="1.5" class="key-shadow"/><rect x={key.x-key.w/2} y={key.y-key.h/2} width={key.w} height={key.h} rx="1.5" class="key-body"/><rect x={key.x-key.w/2+.7} y={key.y-key.h/2+.56} width={key.w-1.4} height={key.h-1.68} rx="1.1" class="key-face"/><Show when={key.legend}><text class="key-legend" x={key.x} y={key.y+.4}>{key.legend}</text></Show><Show when={key.style === 'trig'}><rect class="trig-underline" x={key.x-2.4} y={key.y+2.9} width="4.8" height=".7" rx=".15"/></Show><Show when={key.icon}><g transform={`translate(${key.x-3} ${key.y-3}) scale(.25)`}><Icon name={key.icon!}/></g></Show><Show when={key.sub}><text class="key-sub" x={key.x} y={key.sub_y ?? key.y + key.h/2 + 3.5}>{key.sub}</text></Show></g>}</For>
    </svg><canvas ref={canvas} class="oled" width="128" height="64" aria-label="128 by 64 OLED display" style={{ left: `${panel().glass[0] / 215 * 100}%`, top: `${panel().glass[1] / 176 * 100}%`, width: `${(panel().glass[2] - panel().glass[0]) / 215 * 100}%`, height: `${(panel().glass[3] - panel().glass[1]) / 176 * 100}%` }} /></div></section>
  </main>;
}
