// The Profile pane: where the ColdFire time goes, per module, per runtime phase and per poll, and on the emulator the DSP per block. A tab of the
// right-hand column (SidePanels.tsx) of the Emulator page and of the Remote page, in the Card panel's look (card-panel.css) and the Memory panel's
// tokens (profile-panel.css). The pure rules (rows, shares, the graph history, the hot rule) are packages/workbench-core/src/profile/profile-core.ts;
// what is read and done is profile-backend.ts (the recorder), over the target's one session (target.ts).
// Reads: once a second, only while the tab is shown and the target records, one at a time (read-gate-core.ts); a tab that is shown while the target
// is stopped reads once. Start and Stop, Reset and Export are the recorder's. A target without the debug module gets one button that adds it.
import { For, Show, createEffect, createMemo, createSignal, onCleanup, untrack, type JSX } from 'solid-js';
import { panels } from '@digi/workbench-core/data/faceplates.ts';
import { isCancelled, isNoRuntime } from '@digi/workbench-core/modules/backend.ts';
import type { DkSession } from '@digi/dk-session';
import type { Host } from '@digi/workbench-core/host/host.ts';
import { useTransferApp } from '@digi/workbench-core/transfer/app-context.ts';
import { sameTarget } from '@digi/workbench-core/transfer/target.ts';
import { failureLine } from '@digi/workbench-core/transfer/wait-core.ts';
import {
  buildRows,
  createHistory,
  dspText,
  dspWidth,
  fmtElapsed,
  fmtMhz,
  fmtShare,
  fmtUs,
  graphTop,
  groupRows,
  readReference,
  STACK,
  totalShare,
  type DspView,
  type OwnerKind,
} from '@digi/workbench-core/profile/profile-core.ts';
import { cardRefresh } from './card-state';
import { createDkProfLoader } from './dkprof-env';
import { createEmulatorNames } from './log-names-backend';
import { deviceOfPort } from './remote-core';
import { createRecorder, type ProfileReading } from './profile-backend';
import { createReadGate } from './read-gate-core';
import './card-panel.css';
import './profile-panel.css';

const DEBUG_MODULE = 'digikit/debug';
const LEGEND: { kind: OwnerKind; text: string }[] = [
  { kind: 'machine', text: 'machine' },
  { kind: 'effect', text: 'effect' },
  { kind: 'tool', text: 'tool' },
  { kind: 'runtime', text: 'runtime' },
];
const COLUMN_PX = 4;
const GAP_PX = 2;
const GRAPH_PX = 90;

/**
 * `session`: the device to read (the emulator, or the selected target). `where`: its name for an error line. `track`: read again when this
 * changes (the selected target). `host`: the controller's rpc for `cf.profile` and `sharc.profile` and the save dialog.
 */
export default function ProfilePanel(props: {
  visible: boolean;
  session: () => Promise<DkSession>;
  where: () => string;
  track?: () => unknown;
  host?: Host;
  tabpanel?: { id: string; labelledBy: string };
}): JSX.Element {
  const app = useTransferApp();
  const { asked, noteReply } = app.connection;
  const { checkRuntime, emulatorRuntime, target } = app.target;
  const [reading, setReading] = createSignal<ProfileReading>();
  const [dsp, setDsp] = createSignal<DspView>();
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [acting, setActing] = createSignal('');
  const [noRuntime, setNoRuntime] = createSignal(false);
  const [reference, setReference] = createSignal<{ name: string; shares: Map<string, number> }>();
  const history = createHistory();
  const [version, setVersion] = createSignal(0); // the history changed

  const readsEmulator = () => props.where() === 'the emulator';
  const readsTarget = () => !readsEmulator() || target().kind === 'emulator';
  const onEmulator = () => readsEmulator() || target().kind === 'emulator';

  const prof = props.host ? createDkProfLoader(props.host) : undefined;
  const recorder = createRecorder({
    session: async () => props.session(),
    get rpc() {
      return onEmulator() ? props.host?.rpc : undefined;
    },
    loadLibrary: app.loadLibrary,
    prof,
    names: props.host && prof ? createEmulatorNames(props.host, prof) : undefined,
  });

  const running = () => {
    const r = reading();
    return !!r && !r.noDebug && r.snap.running;
  };

  const gate = createReadGate({
    run: async (current) => {
      setBusy(true);
      const t0 = target();
      try {
        const check = () => checkRuntime(readsEmulator() ? { kind: 'emulator' } : target());
        if (readsTarget()) await asked(async () => check());
        else check();
        const r = readsTarget() ? await asked(() => recorder.read()) : await recorder.read();
        if (current()) {
          if (!r.noDebug) {
            history.push(r.snap, r.modules);
            setVersion((v) => v + 1);
            if (r.dsp) setDsp(r.dsp);
          }
          setReading(r);
          setError('');
          setNoRuntime(false);
          if (readsTarget()) void noteReply(await props.session(), () => sameTarget(t0, target()));
        }
        return true;
      } catch (e) {
        if (!current()) return true;
        if (isCancelled(e)) return true;
        if (isNoRuntime(e)) {
          setError(failureLine(e, props.where()));
          setNoRuntime(true);
          return true;
        }
        setError(failureLine(e, props.where()));
        return false;
      } finally {
        setBusy(false);
      }
    },
  });

  const active = createMemo(() => props.visible);
  // read when the tab is shown, when the target changed, when a Transfer job ended or the emulator restarted
  createEffect(() => {
    cardRefresh();
    props.track?.();
    emulatorRuntime();
    untrack(() => {
      history.clear();
      setReading(undefined);
      setDsp(undefined);
      setVersion((v) => v + 1);
    });
  });
  createEffect(() => {
    cardRefresh();
    props.track?.();
    emulatorRuntime();
    if (active()) untrack(() => gate.request());
    else untrack(() => gate.stop());
  });
  // while the target records and the tab is shown: once a second
  createEffect(() => {
    if (!active() || !running()) return;
    const id = setInterval(() => gate.request(), 1000);
    onCleanup(() => clearInterval(id));
  });
  onCleanup(() => gate.dispose());

  const act = async (what: string, f: () => Promise<void>) => {
    setActing(what);
    try {
      await f();
      setError('');
      gate.request();
    } catch (e) {
      setError(failureLine(e, props.where()));
    } finally {
      setActing('');
    }
  };
  const start = () =>
    act('start', async () => {
      history.clear();
      setDsp(undefined);
      await recorder.start();
    });
  const stop = () => act('stop', () => recorder.stop());
  const reset = () =>
    act('reset', async () => {
      history.clear();
      setDsp(undefined);
      await recorder.reset();
    });
  const exportFile = () =>
    act('export', async () => {
      if (!props.host) return;
      const f = await recorder.exportFile();
      await props.host.fs.saveLog(new TextEncoder().encode(f.text), f.name, 'profile');
    });
  const addDebug = () =>
    act('add', async () => {
      // the debug module is in the library; the runtime loads it at the next start
      await app.backend.install([DEBUG_MODULE], () => {});
      await app.backend.restart(() => {});
    });
  const openReference = async (file: File | undefined) => {
    if (!file) return;
    try {
      const shares = readReference(JSON.parse(await file.text()));
      if (!shares) setError(`${file.name} has no spans or no elapsed window (an export of this pane has both).`);
      else {
        setReference({ name: file.name, shares });
        setError('');
      }
    } catch (e) {
      setError(`${file.name}: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  const ok = () => {
    const r = reading();
    return r && !r.noDebug ? r : undefined;
  };
  const rows = createMemo(() => {
    const r = ok();
    return r ? buildRows(r.snap, r.modules) : [];
  });
  const groups = createMemo(() => groupRows(rows()));
  const cols = createMemo(() => (version(), history.columns()));
  const top = createMemo(() => graphTop(cols()));
  const marks = createMemo(() => (version(), history.markers()).slice(-3));
  const nowShare = createMemo(() => (version(), history.now()));
  /** `Digitone II 1.12`: the device and the firmware version the port name carries (`dn2-1.12`). */
  const device = () => {
    const port = reading()?.port ?? '';
    const d = deviceOfPort(port);
    const version = port.slice(port.indexOf('-') + 1);
    return d ? `${panels[d].name}${version && version !== port ? ` ${version}` : ''}` : '';
  };
  /** What the reading came from: the emulator (a controller to ask for the instruction profiles) or a device. */
  const emu = () => reading()?.emulator ?? onEmulator();
  const where = () => (emu() ? 'emulator' : 'USB');
  const px = (share: number) => Math.round((share / top()) * GRAPH_PX);
  const barPx = (share: number) => Math.min(36, Math.max(2, Math.round(share * 100 * 1.6)));
  const kindClass = (k: OwnerKind) => `k-${k}`;
  const dspSegments = (v: DspView) => v.modules.map((m) => ({ ...m, width: dspWidth(v, m.insnPerBlock) }));

  return (
    <aside
      class="card-panel profile-panel"
      id={props.tabpanel?.id}
      role={props.tabpanel ? 'tabpanel' : undefined}
      aria-labelledby={props.tabpanel?.labelledBy}
      aria-label={props.tabpanel ? undefined : 'Profile'}
      hidden={!props.visible}
    >
      <div class="card-body">
        <Show when={device()}>
          <h3 class="device">
            {device()} · {where()}
          </h3>
        </Show>
        <Show when={reading()?.noDebug}>
          <div class="prof-skeleton" aria-hidden="true">
            <div class="graph" />
            <span class="line" style={{ width: '70%' }} />
            <span class="line" style={{ width: '55%' }} />
            <span class="line" style={{ width: '62%' }} />
          </div>
          <div class="prof-nodebug">
            <strong>Profiling needs the debug module</strong>
            <span class="text">
              The debug module counts the runtime's spans on the device. Without it the runtime pays one flag test per
              span and records nothing.
            </span>
            <div class="acts">
              <button type="button" class="act primary" disabled={!!acting()} onClick={() => void addDebug()}>
                {acting() === 'add' ? 'Adding…' : 'Add debug module'}
              </button>
            </div>
            <span class="hint">Uploads it with Transfer. The device restarts once.</span>
          </div>
        </Show>
        <Show when={ok()}>
          {(r) => (
            <>
              <div class="prof-head">
                <div class="line1">
                  <span class="state-line">
                    <span class="dot" classList={{ rec: r().snap.running }} />
                    {r().snap.running ? 'Recording' : r().snap.elapsedTicks > 0n ? 'Stopped after ' : 'Stopped'}
                    {!r().snap.running && r().snap.elapsedTicks > 0n
                      ? fmtElapsed(r().snap.elapsedTicks, r().snap.hz)
                      : ''}
                    {' · clock '}
                    {fmtMhz(r().snap.hz)} {emu() ? '(emulated time)' : '(DTIM0)'}
                  </span>
                  <span class="elapsed">{r().snap.running ? fmtElapsed(r().snap.elapsedTicks, r().snap.hz) : ''}</span>
                </div>
                <div class="acts">
                  <Show
                    when={r().snap.running}
                    fallback={
                      <button type="button" class="act primary" disabled={!!acting()} onClick={() => void start()}>
                        Start
                      </button>
                    }
                  >
                    <button type="button" class="act" disabled={!!acting()} onClick={() => void stop()}>
                      Stop
                    </button>
                  </Show>
                  <button type="button" class="act quiet" disabled={!!acting()} onClick={() => void reset()}>
                    Reset
                  </button>
                  <span class="sp" />
                  <label class="act quiet file">
                    Open profile…
                    <input
                      type="file"
                      accept=".json,application/json"
                      onChange={(e) => {
                        void openReference(e.currentTarget.files?.[0]);
                        e.currentTarget.value = '';
                      }}
                    />
                  </label>
                  <button
                    type="button"
                    class="act quiet"
                    disabled={!!acting() || !props.host || r().snap.elapsedTicks === 0n}
                    onClick={() => void exportFile()}
                  >
                    Export…
                  </button>
                </div>
              </div>

              <div class="prof-sec">
                <div class="sec-head">
                  <span class="title">{emu() ? 'ColdFire, last 60 s' : 'ColdFire, recorded window'}</span>
                  <span class="num">
                    {emu() ? `${fmtShare(nowShare())} now` : `${fmtShare(totalShare(r().snap))} mean`}
                  </span>
                </div>
                <div class="graph" role="img" aria-label="ColdFire share per owner type over time">
                  <For each={cols()}>
                    {(c) => (
                      <div class="col">
                        <For each={STACK}>
                          {(k) => <div class={kindClass(k)} style={{ height: `${px(c[k])}px` }} />}
                        </For>
                      </div>
                    )}
                  </For>
                  <For each={marks()}>
                    {(m) => (
                      <>
                        <div class="mark" style={{ left: `${6 + m.col * (COLUMN_PX + GAP_PX)}px` }} />
                        <span class="mark-text" style={{ left: `${10 + m.col * (COLUMN_PX + GAP_PX)}px` }}>
                          {m.text}
                        </span>
                      </>
                    )}
                  </For>
                  <span class="top">{Math.round(top() * 100)}%</span>
                </div>
                <ul class="legend" aria-label="Owner types">
                  <For each={LEGEND}>
                    {(l) => (
                      <li>
                        <span class={`swatch ${kindClass(l.kind)}`} />
                        {l.text}
                      </li>
                    )}
                  </For>
                </ul>
              </div>

              <div class="prof-table" classList={{ cmp: !!reference() }}>
                <div class="row colhead">
                  <span>Owner</span>
                  <span>Share</span>
                  <span class="r">mean µs</span>
                  <span class="r">max µs</span>
                  <Show when={reference()}>
                    <span class="r" title={reference()!.name}>
                      file
                    </span>
                  </Show>
                </div>
                <Show when={groups().length === 0}>
                  <p class="empty">
                    {r().snap.running ? 'Waiting for the first spans…' : 'Nothing recorded. Press Start.'}
                  </p>
                </Show>
                <For each={groups()}>
                  {(g) => (
                    <>
                      <div class="group">{g.label}</div>
                      <For each={g.rows}>
                        {(row) => (
                          <div class="row">
                            <span class="owner">
                              <span class={`swatch ${kindClass(row.kind)}`} />
                              <span class="name" title={row.name}>
                                {row.name}
                              </span>
                            </span>
                            <span class="share">
                              <span class={`bar ${kindClass(row.kind)}`} style={{ width: `${barPx(row.share)}px` }} />
                              <span class="pct">{fmtShare(row.share)}</span>
                            </span>
                            <span class="r mono">{fmtUs(row.meanUs)}</span>
                            <span
                              class="r mono"
                              classList={{ hot: row.hot }}
                              title={row.hot ? 'The longest call is far above the mean' : undefined}
                            >
                              {fmtUs(row.maxUs)}
                            </span>
                            <Show when={reference()}>
                              <span class="r mono dim">
                                {reference()!.shares.has(row.name) ? fmtShare(reference()!.shares.get(row.name)!) : '–'}
                              </span>
                            </Show>
                          </div>
                        )}
                      </For>
                    </>
                  )}
                </For>
              </div>

              <Show when={emu()}>
                <div class="prof-sec ruled">
                  <div class="sec-head">
                    <span class="title">DSP per block (emulator)</span>
                    <Show when={dsp()}>{(v) => <span class="num">{dspText(v())}</span>}</Show>
                  </div>
                  <Show
                    when={dsp()}
                    fallback={
                      <span class="hint">
                        {r().note ??
                          (r().snap.running
                            ? 'Waiting for the DSP counts…'
                            : 'Start records the DSP counts of the emulator with the spans.')}
                      </span>
                    }
                  >
                    {(v) => (
                      <>
                        <div class="dspbar" role="img" aria-label={`DSP per block: ${dspText(v())}`}>
                          <For each={dspSegments(v())}>
                            {(m) => <div class={kindClass(m.kind)} style={{ width: `${m.width}%` }} title={m.name} />}
                          </For>
                        </div>
                        <span class="hint">
                          Module instructions per DSP block
                          {v().headroom ? ' against the device headroom' : ''}. Stock DSP work is outside the bar.
                        </span>
                        <div class="dsp-mods">
                          <For each={v().modules}>
                            {(m) => (
                              <>
                                <div class="dsp-mod">
                                  <span class={`swatch ${kindClass(m.kind)}`} />
                                  <span class="name">{m.name}</span>
                                  <span class="mono">{Math.round(m.insnPerBlock).toLocaleString('en-US')}</span>
                                </div>
                                <div class="dsp-top">
                                  <For each={m.top}>
                                    {(t) => (
                                      <div>
                                        <span class="name">{t.label}</span>
                                        <span class="mono">{t.pct}%</span>
                                      </div>
                                    )}
                                  </For>
                                </div>
                              </>
                            )}
                          </For>
                        </div>
                      </>
                    )}
                  </Show>
                  <Show when={dsp() && r().note}>
                    <span class="hint">{r().note}</span>
                  </Show>
                </div>
              </Show>
              <Show when={!emu()}>
                <div class="prof-sec ruled">
                  <div class="sec-head">
                    <span class="title">DSP, whole block (HEALTH)</span>
                    <Show when={r().deviceDsp}>
                      {(d) => (
                        <span class="num">
                          {d().avgPct.toFixed(1)}% mean · {d().maxPct.toFixed(1)}% max
                        </span>
                      )}
                    </Show>
                  </div>
                  <Show when={r().deviceDsp}>
                    {(d) => (
                      <div class="dspbar" role="img" aria-label={`DSP block load: ${d().avgPct.toFixed(1)}% mean`}>
                        <div class="k-machine" style={{ width: `${Math.min(100, d().avgPct)}%` }} />
                        <div
                          class="peak"
                          style={{ width: `${Math.max(0, Math.min(100, d().maxPct) - Math.min(100, d().avgPct))}%` }}
                        />
                      </div>
                    )}
                  </Show>
                  <span class="hint">
                    {r().deviceDsp
                      ? 'The whole block as the runtime logs it (HEALTH).'
                      : 'No HEALTH event in the ring yet.'}{' '}
                    Per-module DSP needs the DSP stats window.
                  </span>
                </div>
              </Show>
              <span class="foot">
                Reads every second while this tab is open. Export writes dk.profile/1 (
                {emu() ? 'spans, cf.profile, sharc.profile' : 'spans'}).
              </span>
            </>
          )}
        </Show>
        <Show when={error()}>
          <p class="err" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={noRuntime() && !reading()}>
          <p class="empty">The runtime is not loaded on this target.</p>
        </Show>
        <Show when={busy() && !reading() && !error()}>
          <p class="empty">Reading…</p>
        </Show>
      </div>
    </aside>
  );
}
