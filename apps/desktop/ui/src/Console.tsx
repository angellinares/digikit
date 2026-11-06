// The desktop developer console: the file the app runs, the live panel, and the audio and CPU meters.
// `mise run workbench -- FILE` starts it on FILE; with "watching" on, a change to FILE on disk reloads it, so a
// module can be patched from the command line and heard here a second later.
import { invoke } from '@tauri-apps/api/core';
import { Show, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js';
import LivePanel, { type LivePanelApi } from '@web/components/LivePanel';
import { panels } from '@web/data/faceplates';
import {
  nativeRuntime,
  type EmulatorRuntime,
  type FirmwareInfo,
  type NativeAudioStatus,
  type RuntimeSnapshot,
  type RuntimeUpdate,
} from '@web/runtime/runtime';
import { bumpCardRefresh } from './card-state';
import { DOCK_KEY_STEP, clampDock, dockAt, parseDock, startDock } from './dock-core';
import {
  duration,
  failureEntry,
  firmwareEntry,
  healthChanges,
  healthStart,
  reloadEntry,
  reloadFailureEntry,
  unseenErrors,
} from './log-core';
import { desktopDefaultModules } from './default-modules';
import { pgoWarning, type BuildInfo } from './pgo-warning';
import { createLogFollow, followSession } from './log-follow';
import { windowShown } from './log-follow-core';
import { followOn, logEvent, logInput, logOpen, logState, setLogOpen } from './log-state';
import LogPanel from './LogPanel';
import SidePanels from './SidePanels';
import RemotePage from './RemotePage';
import { emulatorSession, lastMidiPort, lastMidiSession, target } from '@web/transfer/target';
import { transferEnv } from '@web/transfer/env';
import { run as flashRun } from '@web/firmware/firmware-state';
import { running as flashRunning } from '@web/firmware/firmware-core';
import FirmwarePage from '@web/firmware/FirmwarePage';
import DefaultModules from '@web/firmware/DefaultModules';
import { DefaultModuleError } from '@web/firmware/default-modules-core';
import { tickedFor } from '@web/firmware/default-modules-state';
import TransferPage from '@web/transfer/TransferPage';
import { cardLine, folderFiles, startCard, type CardStatus } from '@web/runtime/cards';
import { reloadError, reloadLine } from '@web/runtime/reload';

type Source = { path: string; name: string; bytes: number; modified_ms: number };
type Status = RuntimeSnapshot['status'];

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

// "Boot with digikit runtime": on unless the user turned it off; kept per browser profile of the app.
const RUNTIME_KEY = 'digikit.bootRuntime';
const readRuntimePref = () => {
  try {
    return localStorage.getItem(RUNTIME_KEY) !== 'off';
  } catch {
    return true;
  }
};
const writeRuntimePref = (on: boolean) => {
  try {
    localStorage.setItem(RUNTIME_KEY, on ? 'on' : 'off');
  } catch {
    /* the choice lasts until the window closes */
  }
};

// The height of the Log dock in px; kept per browser profile of the app (none saved: a share of the window).
const DOCK_KEY = 'digikit.logHeight';
const readDockPref = () => {
  try {
    return parseDock(localStorage.getItem(DOCK_KEY));
  } catch {
    return undefined;
  }
};
const writeDockPref = (px: number | undefined) => {
  try {
    if (px === undefined) localStorage.removeItem(DOCK_KEY);
    else localStorage.setItem(DOCK_KEY, String(px));
  } catch {
    /* the height lasts until the window closes */
  }
};

// The sidebar is open unless the user collapsed it; kept per browser profile of the app.
const SIDEBAR_KEY = 'digikit.sidebar';
const readSidebarPref = () => {
  try {
    return localStorage.getItem(SIDEBAR_KEY) !== 'closed';
  } catch {
    return true;
  }
};
const writeSidebarPref = (open: boolean) => {
  try {
    localStorage.setItem(SIDEBAR_KEY, open ? 'open' : 'closed');
  } catch {
    /* the choice lasts until the window closes */
  }
};

function Icon(props: { d: string }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path
        d={props.d}
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}
const FOLDER = 'M1.5 4V12.5H14.5V5.5H7.5L6 4Z';
const RELOAD = 'M13 3V6.5H9.5M13 6.5A5.2 5.2 0 1 0 13.4 10';
const PAUSE = 'M5.5 3V13M10.5 3V13';
const PLAY = 'M4.5 2.5L12.5 8L4.5 13.5Z';
const RESTART = 'M3 2.5V6H6.5M3 6A5.2 5.2 0 1 1 2.8 9.8';
const CHART = 'M2 12.5L5.5 7.5L8.5 10L13.5 3.5M10.5 3.5H13.5V6.5';
const SPEAKER = 'M2.5 6H5L8.5 3V13L5 10H2.5ZM11 5.5a3.5 3.5 0 0 1 0 5';
const WARNING = 'M8 2.5L14.5 13.5H1.5ZM8 6.5V9.5M8 11.5V11.6';
const UP = 'M4 10L8 6L12 10';
const REMOTE = 'M2.5 3.5H13.5V10.5H2.5ZM6 13H10M8 10.5V13';
// a chip with pins
const FIRMWARE =
  'M4.5 4.5H11.5V11.5H4.5ZM6.5 2.5V4.5M9.5 2.5V4.5M6.5 11.5V13.5M9.5 11.5V13.5M2.5 6.5H4.5M2.5 9.5H4.5M11.5 6.5H13.5M11.5 9.5H13.5';
const MODULES = 'M2.5 5L8 2.5L13.5 5V11L8 13.5L2.5 11ZM2.5 5L8 7.5L13.5 5M8 7.5V13.5';
const DOWN = 'M4 6L8 10L12 6';

function Meter(props: { value: number; worst?: number; tone?: 'teal' | 'amber' }) {
  const pct = (v: number) => `${Math.max(0, Math.min(100, v))}%`;
  return (
    <span class="meter" aria-hidden="true">
      <b class={props.tone ?? 'teal'} style={{ width: pct(props.value) }} />
      <Show when={props.worst !== undefined}>
        <u style={{ left: pct(props.worst!) }} />
      </Show>
    </span>
  );
}

export default function Console() {
  const [runtime, setRuntime] = createSignal<EmulatorRuntime>();
  const [outsideApp, setOutsideApp] = createSignal(false);
  const [status, setStatus] = createSignal<Status>();
  const [error, setError] = createSignal('');
  const [paused, setPaused] = createSignal(false);
  const [source, setSource] = createSignal<Source>();
  const [opened, setOpened] = createSignal('');
  const [watching, setWatching] = createSignal(true);
  const [reloadNote, setReloadNote] = createSignal('');
  const [modulesBusy, setModulesBusy] = createSignal(false);
  const [audio, setAudio] = createSignal<NativeAudioStatus>();
  const [buildInfo, setBuildInfo] = createSignal<BuildInfo>();
  const [rate, setRate] = createSignal<number>();
  const [drawer, setDrawer] = createSignal(false);
  // the Log drawer: the last entry seen (the badge counts the errors after it); the filter and the list are in LogPanel.tsx
  const [seenId, setSeenId] = createSignal(0);
  // the Log dock: its height (undefined: the default share of the window), the window height, and a drag in progress
  const [dockSaved, setDockSaved] = createSignal(readDockPref());
  const [windowPx, setWindowPx] = createSignal(window.innerHeight);
  const [dockDrag, setDockDrag] = createSignal(false);
  const [page, setPage] = createSignal<'emulator' | 'transfer' | 'remote' | 'firmware'>('emulator');
  const [sidebarOpen, setSidebarOpen] = createSignal(readSidebarPref());
  const [card, setCard] = createSignal<CardStatus>();
  const [cardBusy, setCardBusy] = createSignal(false);
  const [bootRuntime, setBootRuntime] = createSignal(readRuntimePref());
  const [info, setInfo] = createSignal<FirmwareInfo>();
  let live: LivePanelApi | undefined;
  let fileInput: HTMLInputElement | undefined;
  let folderInput: HTMLInputElement | undefined;
  let lastAudio: { produced: number; wall: number } | undefined;
  let lastModified: number | undefined;
  let reloading = false;
  let health = healthStart();
  let dockEl: HTMLElement | undefined;
  const timers: number[] = [];

  // a memo: `status` is set once per display frame, and the panels must hear of a change of this value only
  const sidePanelsShown = createMemo(() => page() === 'emulator' && !!status());
  const device = () => {
    const d = status()?.device;
    return d === 'dt2' || d === 'dn2' ? d : 'dn2';
  };
  const panel = () => panels[device()];

  const update = (event: RuntimeUpdate) => {
    if (event.type === 'error') {
      setError(event.error);
      return;
    }
    live?.show(event.snapshot);
    setStatus(event.snapshot.status);
    setError(event.snapshot.status.error ?? '');
    watchHealth({ error: event.snapshot.status.error });
  };
  /** Log a guest fault, stall or DSP halt when it starts (the app polls the status; it gets no events from the controller). */
  const watchHealth = (reading: Parameters<typeof healthChanges>[1]) => {
    const seen = healthChanges(health, reading);
    health = seen.health;
    seen.entries.forEach(logInput);
  };

  /** Set when a default-modules install has started since the app opened (boot() or the startup check). */
  let defaultsStarted = false;
  /** What the running image got from the runtime patch, for the header. */
  const refreshInfo = async () => {
    try {
      setInfo(await runtime()?.firmwareInfo?.());
    } catch {
      setInfo(undefined);
    }
  };
  /**
   * The ticked default modules of the device (devices/*.toml `[digikit] default_modules`) go on the emulator's card and are enabled, so the
   * emulator has what a flashed device has. The card keeps them: a later load finds them there and only checks the files.
   */
  const installDefaultModules = async (deviceId: string) => {
    const ids = tickedFor(deviceId);
    if (ids.length === 0) return;
    try {
      const lines = await desktopDefaultModules.install('emulator', deviceId, ids, (p) =>
        setReloadNote(`Default modules: ${p.text}`),
      );
      setReloadNote(`Default modules ready: ${ids.join(', ')}`);
      logInput({
        source: 'firmware',
        level: 'info',
        text: `default modules in the emulator: ${ids.join(', ')}`,
        detail: lines.join('\n'),
      });
    } catch (e) {
      const what = e instanceof DefaultModuleError ? `default module ${e.id}` : 'default modules';
      setReloadNote(`${what} failed`);
      logInput(failureEntry('firmware', what, e instanceof DefaultModuleError ? e.message : message(e)));
    }
    bumpCardRefresh();
  };
  /** Start a new firmware session with `start` (every load goes through the controller's `firmware.load`). */
  const boot = async (start: (r: EmulatorRuntime) => Promise<RuntimeSnapshot>, name: string) => {
    const r = runtime();
    if (!r) return;
    live?.releaseAll();
    live?.clear();
    setError('');
    health = healthStart();
    const began = performance.now();
    try {
      update({ type: 'snapshot', snapshot: await start(r) });
      setPaused(false);
      await refreshInfo();
      const s = status();
      if (s)
        logInput(
          firmwareEntry({
            name,
            device: s.device,
            version: s.version,
            runtime: bootRuntime(),
            patch: info()?.runtime,
            ms: performance.now() - began,
          }),
        );
      // another firmware may be another device: its card folder and its memory (the runtime needs a moment to load its modules)
      bumpCardRefresh();
      window.setTimeout(bumpCardRefresh, 2500);
      if (s && bootRuntime()) {
        defaultsStarted = true;
        void installDefaultModules(s.device);
      }
    } catch (e) {
      setError(message(e));
      logInput(failureEntry('firmware', `loading ${name}`, message(e)));
    }
  };
  const load = (bytes: Uint8Array, name: string) => boot((r) => r.load(bytes, name, { runtime: bootRuntime() }), name);
  /** Boot the startup file again: the controller reads it from its path. */
  const reload = async (why: string) => {
    const path = source()?.path;
    if (reloading || !path) return;
    reloading = true;
    try {
      setOpened('');
      await boot(
        (r) => (r.loadPath ? r.loadPath(path, { runtime: bootRuntime() }) : Promise.reject(new Error('no reload'))),
        source()?.name ?? path,
      );
      setReloadNote(`${why} ${new Date().toLocaleTimeString()}`);
    } catch (e) {
      setError(message(e));
    } finally {
      reloading = false;
    }
  };
  /** `dev.reload {build: true}`: the controller builds the stale ColdFire objects, applies the modules again from the objects on disk
   * and swaps module code into a restored main screen, or boots the new image; the result goes to the status area and the Log. A build
   * error is one short line in the status area; the Log has it with the compiler output as its detail. */
  const reloadModules = async () => {
    const r = runtime();
    if (!r?.reloadModules || modulesBusy()) return;
    setModulesBusy(true);
    setError('');
    setReloadNote('Reloading modules…');
    try {
      const report = await r.reloadModules();
      setReloadNote(reloadLine(report));
      logInput(reloadEntry(report));
      health = healthStart();
      setPaused(false);
      await refreshInfo();
    } catch (e) {
      const failure = reloadError(e);
      setReloadNote(failure.status);
      logInput(reloadFailureEntry(failure));
    } finally {
      setModulesBusy(false);
    }
  };
  const pollSource = async () => {
    try {
      const file = await invoke<Source | null>('emu_source_info');
      setSource(file ?? undefined);
      if (!file) return;
      const changed = lastModified !== undefined && file.modified_ms !== lastModified;
      lastModified = file.modified_ms;
      if (changed && watching() && !opened()) await reload('Reloaded after a change at');
    } catch {
      /* the file may be mid-write; the next poll sees it */
    }
  };
  const pollAudio = async () => {
    try {
      const a = await runtime()?.nativeAudio?.();
      setAudio(a);
      watchHealth({ halted: a?.halted ?? null });
      const produced = a?.source_audio_seconds,
        wall = a?.session_wall_seconds;
      if (produced === undefined || wall === undefined) return;
      if (lastAudio && produced < lastAudio.produced) lastAudio = undefined;
      if (lastAudio && wall > lastAudio.wall) setRate((produced - lastAudio.produced) / (wall - lastAudio.wall));
      lastAudio = { produced, wall };
    } catch {
      /* a replaced session has no status */
    }
  };

  /** The heartbeat, once a second: a stalled main loop shows nowhere else. */
  const pollHealth = async () => {
    try {
      const beat = await runtime()?.health?.();
      if (beat) watchHealth({ stalled: beat.stalled, stallMs: beat.stall_ms });
    } catch {
      /* a replaced session has no status */
    }
  };

  const toggleLog = () => setLogOpen(!logOpen());
  const toggleMetrics = () => setDrawer(!drawer());
  const dockPx = () => startDock(dockSaved(), windowPx());
  const dockDragTo = (clientY: number) => {
    if (!dockEl) return;
    setDockSaved(dockAt(clientY, dockEl.getBoundingClientRect().bottom, windowPx()));
  };
  const dockNudge = (delta: number) => {
    setDockSaved(clampDock(dockPx() + delta, windowPx()));
    writeDockPref(dockSaved());
  };
  const onResize = () => setWindowPx(window.innerHeight);
  const onKey = (e: KeyboardEvent) => {
    // ⌘J / Ctrl+J opens and closes the Log.
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'j') {
      e.preventDefault();
      toggleLog();
      return;
    }
    // ⌘R / Ctrl+R reloads the firmware file, not the page; with Shift it reloads the modules into the running session.
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'r') {
      e.preventDefault();
      if (e.shiftKey) void reloadModules();
      else void reload('Reloaded at');
    }
  };
  onMount(() => {
    void nativeRuntime(update).then((r) => {
      if (!r) {
        setOutsideApp(true);
        return;
      }
      setRuntime(r);
      void invoke<BuildInfo>('emu_build_info')
        .then(setBuildInfo)
        .catch(() => setBuildInfo(undefined));
      void (async () => {
        await pollSource();
        await refreshInfo();
        // The app starts with the runtime patched in; a user who turned that off gets the startup file as it is.
        if (!bootRuntime() && info()?.runtime?.set) await reload('Started without the runtime at');
        // The launcher may have started the session with the runtime already: boot() did not run, so install the default modules here.
        // Not when boot() already started the install (a load during startup).
        const s = status();
        if (s && bootRuntime() && info()?.runtime?.set && !defaultsStarted) {
          defaultsStarted = true;
          void installDefaultModules(s.device);
        }
      })();
      timers.push(window.setInterval(() => void pollSource(), 1000));
      timers.push(window.setInterval(() => void pollAudio(), 1000));
      timers.push(window.setInterval(() => void pollHealth(), 1000));
    });
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onResize);
  });
  onCleanup(() => {
    timers.forEach((t) => window.clearInterval(t));
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('resize', onResize);
    void runtime()?.stop();
    runtime()?.dispose();
  });

  const togglePause = () => {
    const r = runtime();
    if (!r) return;
    if (paused()) r.resume();
    else r.pause();
    setPaused(!paused());
    logEvent('emulator', 'info', paused() ? 'paused' : 'resumed');
  };
  const restart = async () => {
    health = healthStart();
    const began = performance.now();
    try {
      const s = await runtime()?.restart();
      if (s) update({ type: 'snapshot', snapshot: s });
      setPaused(false);
      await refreshInfo();
      logEvent('reset', 'info', `emulator reset · ${duration(performance.now() - began)}`);
      bumpCardRefresh();
      window.setTimeout(bumpCardRefresh, 2500);
    } catch (e) {
      setError(message(e));
      logInput(failureEntry('reset', 'reset', message(e)));
    }
  };
  const saveDiagnostics = async () => {
    try {
      const r = runtime();
      const report = await r?.diagnostics();
      if (!r?.saveDiagnostics || !report) return;
      const path = await r.saveDiagnostics(
        `digiemu-${report.device}-diagnostics.json`,
        JSON.stringify(report, null, 2),
      );
      setReloadNote(`Diagnostics saved to ${path}`);
      logEvent('emulator', 'info', `diagnostics saved to ${path}`);
    } catch (e) {
      setError(`Diagnostics failed: ${message(e)}`);
      logInput(failureEntry('emulator', 'diagnostics', message(e)));
    }
  };
  const chooseSamples = async (list: FileList | null) => {
    const cards = runtime()?.cards;
    if (!cards || !list) return;
    const files = folderFiles(Array.from(list));
    if (folderInput) folderInput.value = '';
    if (files.length === 0) {
      setCard({ state: 'error', message: 'No files in that folder.' });
      logEvent('card', 'error', 'sample folder: no files in that folder');
      return;
    }
    setCardBusy(true);
    try {
      await startCard(cards, files, (done, total) =>
        setCard({ state: 'working', message: `Sending samples (${done}/${total})`, fraction: 0 }),
      );
      const poll = async () => {
        const s = await runtime()!.cards!.status();
        setCard(s);
        if (s.state === 'working') window.setTimeout(() => void poll(), 400);
        else {
          setCardBusy(false);
          logEvent('card', s.state === 'error' ? 'error' : 'info', cardLine(s));
        }
      };
      void poll();
    } catch (e) {
      setCardBusy(false);
      setCard({ state: 'error', message: message(e) });
      logInput(failureEntry('card', 'sample folder', message(e)));
    }
  };

  // the Log drawer
  const badge = () => (logOpen() ? 0 : unseenErrors(logState().entries, seenId()));
  const mac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
  const logKey = mac ? '⌘J' : 'Ctrl+J';
  // an open drawer has seen everything logged so far
  createEffect(() => {
    const entries = logState().entries;
    if (logOpen()) setSeenId(entries.at(-1)?.id ?? 0);
  });
  // the runtime logs of the emulator and of the device on the MIDI port chosen last (nothing is read while the emulator is paused)
  const logFollow = createLogFollow({
    follow: followOn,
    flashing: () => flashRunning(flashRun()),
    shown: windowShown,
    drawerOpen: logOpen,
    emulator: {
      running: () => !!status() && !paused(),
      session: async () => followSession(await emulatorSession()),
    },
    device: {
      port: lastMidiPort,
      session: async () => followSession(await lastMidiSession()),
      present: async () => (await transferEnv().targets.ports()).includes(lastMidiPort() ?? ''),
    },
  });

  const fileLabel = () => opened() || source()?.path || 'No firmware file';
  /** The last three parts of the path; the full path is the tooltip. */
  const shortPath = () => {
    const parts = fileLabel().split('/');
    return parts.length > 3 ? `…/${parts.slice(-3).join('/')}` : fileLabel();
  };
  const phase = () => {
    const s = status();
    if (!s) return '';
    if (s.frame_revision === 0) return 'booting';
    return s.frame_source === 'intro' ? 'startup display' : s.phase;
  };
  const load_ = () => audio()?.load ?? undefined;
  const sink = () => audio()?.sink ?? undefined;

  return (
    <div
      class="console"
      classList={{ 'dock-dragging': dockDrag() }}
      style={{
        '--accent': panel().accent,
        '--func-legend': panel().func_legend,
        '--dock-h': logOpen() ? `${dockPx()}px` : '0px',
      }}
    >
      <header class="topbar">
        <span class="file" title={fileLabel()}>
          <Icon d={FOLDER} />
          <span class="path">{shortPath()}</span>
        </span>
        <Show when={status()}>
          <span class="tag device">
            {status()!.device} {status()!.version}
          </span>
        </Show>
        <Show when={info()}>
          {(i) => (
            <span
              class="tag runtime"
              classList={{ on: !!i().runtime?.patched }}
              title={
                i().runtime?.patched
                  ? `${i().runtime?.modules?.length ?? 0} modules of the "${i().runtime?.set}" set, ${i().runtime?.cache_hit ? 'from the image cache' : 'patched now'}`
                  : i().runtime
                    ? 'The image has a patch record: it is loaded as it is'
                    : 'The image is loaded as it is'
              }
            >
              {i().runtime?.patched
                ? `runtime ${i().runtime?.set}${i().runtime?.cache_hit ? ' (cached)' : ''}`
                : i().runtime
                  ? 'patched image'
                  : 'stock'}
            </span>
          )}
        </Show>
        <Show when={source() && !opened()}>
          <button
            type="button"
            class="tag watch"
            classList={{ off: !watching() }}
            aria-pressed={watching()}
            title="Reload when the file changes on disk"
            onClick={() => setWatching(!watching())}
          >
            <i />
            {watching() ? 'watching' : 'not watching'}
          </button>
        </Show>
        <Show when={opened()}>
          <span class="tag" title="Opened from the file picker; only the startup file is watched">
            opened
          </span>
        </Show>
        <span class="sp" />
        <label
          class="tag opt"
          title="Patch the digikit runtime into a stock firmware when it is opened or reloaded (the next Open or Reload)"
        >
          <input
            type="checkbox"
            checked={bootRuntime()}
            onChange={(e) => {
              setBootRuntime(e.currentTarget.checked);
              writeRuntimePref(e.currentTarget.checked);
            }}
          />
          <span class="long">Boot with digikit runtime</span>
          <span class="short">Runtime</span>
        </label>
        <input
          ref={fileInput}
          type="file"
          accept=".syx,application/octet-stream"
          hidden
          onChange={async (e) => {
            const f = e.currentTarget.files?.[0];
            e.currentTarget.value = '';
            if (!f) return;
            setOpened(f.name);
            await load(new Uint8Array(await f.arrayBuffer()), f.name);
          }}
        />
        <button
          type="button"
          class="tb"
          aria-label="Open firmware file"
          title="Open a firmware file"
          onClick={() => fileInput?.click()}
        >
          <Icon d={FOLDER} />
          <span class="lbl">Open…</span>
        </button>
        <DefaultModules
          device={device()}
          off={bootRuntime() ? '' : 'Boot with digikit runtime is off: no module can be installed'}
          note="Uploaded and enabled in the emulator after the firmware loads."
        />
        <button
          type="button"
          class="tb"
          disabled={!source()}
          aria-label="Reload"
          title="Reload the startup file (⌘R)"
          onClick={() => void reload('Reloaded at')}
        >
          <Icon d={RELOAD} />
          <span class="lbl">Reload</span>
        </button>
        <button
          type="button"
          class="tb"
          disabled={!status() || modulesBusy()}
          aria-label="Reload modules"
          title="Build the stale module objects, then apply the modules into the running session, without a boot when only UI modules changed (⇧⌘R). A changed SHARC+ source still needs mise run dk:dev."
          onClick={() => void reloadModules()}
        >
          <Icon d={MODULES} />
          <span class="lbl">Reload modules</span>
        </button>
        <button
          type="button"
          class="tb"
          disabled={!status()}
          aria-label={paused() ? 'Resume' : 'Pause'}
          title={paused() ? 'Resume the emulator' : 'Pause the emulator'}
          onClick={togglePause}
        >
          <Icon d={paused() ? PLAY : PAUSE} />
          <span class="lbl">{paused() ? 'Resume' : 'Pause'}</span>
        </button>
        <button
          type="button"
          class="tb"
          disabled={!status()}
          aria-label="Reset (emulator)"
          title="Reset the running firmware to its ready state (the emulator's reset; not the device's RESTART command)"
          onClick={() => void restart()}
        >
          <Icon d={RESTART} />
          <span class="lbl">Reset (emulator)</span>
        </button>
        <button
          type="button"
          class="tb icon"
          disabled={!status()}
          aria-label="Save diagnostics"
          title="Save diagnostics"
          onClick={() => void saveDiagnostics()}
        >
          <Icon d={CHART} />
        </button>
      </header>

      <Show when={error() || outsideApp()}>
        <p class="alert" role="alert">
          {outsideApp() ? 'This page is the desktop app’s console; start it with mise run workbench.' : error()}
        </p>
      </Show>

      <div class="shell">
        <nav class="sidebar" classList={{ closed: !sidebarOpen() }} aria-label="Pages">
          <button
            type="button"
            class="side toggle"
            aria-expanded={sidebarOpen()}
            aria-label={sidebarOpen() ? 'Collapse the sidebar' : 'Expand the sidebar'}
            title={sidebarOpen() ? 'Collapse the sidebar' : 'Expand the sidebar'}
            onClick={() => {
              setSidebarOpen(!sidebarOpen());
              writeSidebarPref(sidebarOpen());
            }}
          >
            <Icon d={sidebarOpen() ? 'M10 3L5 8L10 13' : 'M6 3L11 8L6 13'} />
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'emulator' }}
            aria-current={page() === 'emulator' ? 'page' : undefined}
            title="The emulator"
            onClick={() => setPage('emulator')}
          >
            <Icon d={PLAY} />
            <span>Emulator</span>
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'transfer' }}
            aria-current={page() === 'transfer' ? 'page' : undefined}
            disabled={!status() && target().kind === 'emulator'}
            title="Choose the modules that run on the target (the emulator or a device on a MIDI port) and install them"
            onClick={() => setPage('transfer')}
          >
            <Icon d={MODULES} />
            <span>Transfer</span>
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'remote' }}
            aria-current={page() === 'remote' ? 'page' : undefined}
            disabled={!status() && target().kind === 'emulator'}
            title="Press the keys and turn the encoders of the target, and see its screen"
            onClick={() => setPage('remote')}
          >
            <Icon d={REMOTE} />
            <span>Remote</span>
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'firmware' }}
            aria-current={page() === 'firmware' ? 'page' : undefined}
            title="Patch the stock firmware and flash it to a device over MIDI, or go back to stock"
            onClick={() => setPage('firmware')}
          >
            <Icon d={FIRMWARE} />
            <span>Firmware</span>
          </button>
        </nav>
        <div class="content">
          {/* The faceplate stays mounted behind the Transfer and Remote pages: the emulator, its audio and the snapshot pump run on. */}
          <div class="emu-row" hidden={page() !== 'emulator'}>
            <main class="stage" aria-label={`${panel().name} front panel`}>
              <div class="surface">
                <LivePanel panel={panel()} runtime={runtime} onError={(e) => setError(e)} ref={(api) => (live = api)} />
              </div>
            </main>
            <SidePanels
              visible={sidePanelsShown()}
              card
              reload={restart}
              session={emulatorSession}
              where={() => 'the emulator'}
            />
          </div>
          <Show when={page() === 'transfer'}>
            <TransferPage />
          </Show>
          <Show when={page() === 'remote'}>
            <RemotePage fallbackDevice={device()} />
          </Show>
          <Show when={page() === 'firmware'}>
            <FirmwarePage />
          </Show>
        </div>
      </div>

      <Show when={logOpen()}>
        <section class="log-drawer" aria-label="Log" ref={dockEl} style={{ height: `${dockPx()}px` }}>
          <div
            class="dock-handle"
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize the Log"
            aria-valuemin={0}
            aria-valuenow={dockPx()}
            tabIndex={0}
            onPointerDown={(e) => {
              if (e.button !== 0) return;
              e.currentTarget.setPointerCapture(e.pointerId);
              setDockDrag(true);
            }}
            onPointerMove={(e) => dockDrag() && dockDragTo(e.clientY)}
            onPointerUp={() => {
              if (dockDrag()) writeDockPref(dockSaved());
              setDockDrag(false);
            }}
            onPointerCancel={() => setDockDrag(false)}
            onDblClick={() => {
              setDockSaved(undefined);
              writeDockPref(undefined);
            }}
            onKeyDown={(e) => {
              if (e.key === 'ArrowUp') dockNudge(DOCK_KEY_STEP);
              else if (e.key === 'ArrowDown') dockNudge(-DOCK_KEY_STEP);
              else return;
              e.preventDefault();
            }}
          />
          <LogPanel
            follow={logFollow}
            emulatorRunning={() => !!status()}
            devicePort={lastMidiPort}
            toggleKey={logKey}
          />
        </section>
      </Show>

      <Show when={drawer()}>
        <section class="drawer" aria-label="Metrics">
          <header>
            <strong>Metrics</strong>
            <span>1 s poll</span>
          </header>
          <dl>
            <dt>Firmware</dt>
            <dd>{status() ? `${status()!.device} ${status()!.version} · ${phase()}` : '—'}</dd>
            <dt>Instructions</dt>
            <dd>{status()?.icount.toLocaleString() ?? '—'}</dd>
            <dt>Audio output</dt>
            <dd>{audio()?.output_device ?? (audio()?.setup_needed ? 'no audio for this firmware' : '—')}</dd>
            <dt>Speed</dt>
            <dd>{rate() !== undefined ? `${rate()!.toFixed(2)}× real time (recent)` : '—'}</dd>
            <dt>Underruns</dt>
            <dd classList={{ warn: (sink()?.underrun_events ?? 0) > 0 }}>{sink()?.underrun_events ?? 0}</dd>
            <dt>DSP cost</dt>
            <dd>
              {typeof audio()?.dsp_tiers?.dsp_cpu_per_audio_s === 'number'
                ? `${audio()!.dsp_tiers!.dsp_cpu_per_audio_s!.toFixed(2)} CPU s / audio s`
                : '—'}
            </dd>
            <dt>Produced</dt>
            <dd>{audio() ? `${audio()!.source_audio_seconds.toFixed(2)} s` : '—'}</dd>
            <dt>CF load</dt>
            <dd>{load_() ? `${Math.round(load_()!.cf.real_pct)} %` : '—'}</dd>
            <dt title="Estimate for the real chip. The model figure counts one cycle per instruction, as the on-device HEALTH page does in the emulator.">
              DSP load (est.)
            </dt>
            <dd>
              {load_()?.dsp
                ? `${Math.round(load_()!.dsp!.real_pct)} % est. (worst ${Math.round(load_()!.dsp!.real_high_pct)} %), model ${Math.round(load_()!.dsp!.model_pct)} %`
                : '—'}
            </dd>
            <dt title="Largest estimated DSP load of any window since the meter was reset">DSP window peak</dt>
            <dd>{load_()?.dsp?.peak_pct !== undefined ? `${Math.round(load_()!.dsp!.peak_pct!)} % est.` : '—'}</dd>
          </dl>
          <Show when={reloadNote()}>
            <p class="note">{reloadNote()}</p>
          </Show>
          <Show when={card()}>
            <p class="note">{cardLine(card())}</p>
          </Show>
          <div class="acts">
            <button
              type="button"
              class="tb"
              disabled={!status() || !audio()?.flow_started}
              onClick={() =>
                void runtime()
                  ?.tap?.(25)
                  .catch((e) => setError(message(e)))
              }
            >
              Audition Trig 1
            </button>
            <Show when={device() === 'dt2' && runtime()?.cards}>
              <input
                ref={(el) => {
                  folderInput = el;
                  el.setAttribute('webkitdirectory', '');
                }}
                type="file"
                multiple
                hidden
                onChange={(e) => void chooseSamples(e.currentTarget.files)}
              />
              <button
                type="button"
                class="tb"
                disabled={!status() || cardBusy()}
                title="Pick a plus_drive-style folder: WAVs become samples (track N plays file N, sorted by name), .mmcfs/ and digikit/ are used as they are"
                onClick={() => folderInput?.click()}
              >
                Sample folder…
              </button>
            </Show>
          </div>
        </section>
      </Show>

      <footer class="statusbar">
        <span>
          <i class="dot" classList={{ paused: paused() || !status() }} />
          {!status() ? 'no firmware' : paused() ? 'paused' : phase() || 'running'}
        </span>
        <Show when={audio()?.output_device}>
          <span>
            <Icon d={SPEAKER} />
            {audio()!.output_device}
          </span>
        </Show>
        <Show when={rate() !== undefined}>
          <span title="Emulated audio seconds per wall second">
            {rate()!.toFixed(2)}× RT
            <Meter value={rate()! * 100} tone="amber" />
          </span>
        </Show>
        <Show when={pgoWarning(buildInfo())}>
          {(w) => (
            <span class="warn pgo" title={w().title}>
              <Icon d={WARNING} />
              {w().label}
            </span>
          )}
        </Show>
        <Show when={sink()}>
          <span classList={{ warn: (sink()?.underrun_events ?? 0) > 0 }}>underruns {sink()!.underrun_events}</span>
        </Show>
        <Show when={load_()}>
          <span title="Estimated load of the real chips, from the manuals' cycle tables">
            CF {Math.round(load_()!.cf.real_pct)}%
            <Meter value={load_()!.cf.real_pct} />
          </span>
          <Show when={load_()!.dsp}>
            <span title="Estimated DSP load of the real chip. Model figure (as the HEALTH page shows in the emulator) and window peak are in the drawer">
              DSP est. {Math.round(load_()!.dsp!.real_pct)}%
              <Meter value={load_()!.dsp!.real_pct} worst={load_()!.dsp!.real_high_pct} />
            </span>
          </Show>
        </Show>
        <span class="sp" />
        <Show when={reloadNote() && !drawer()}>
          <span class="muted" title={reloadNote()}>
            {reloadNote()}
          </span>
        </Show>
        <button type="button" class="sb" aria-expanded={logOpen()} title={`Log (${logKey})`} onClick={toggleLog}>
          Log
          <Show when={badge() > 0}>
            <b class="badge" aria-label={`${badge()} new error${badge() === 1 ? '' : 's'}`}>
              {badge()}!
            </b>
          </Show>
          <Icon d={logOpen() ? DOWN : UP} />
        </button>
        <button type="button" class="sb" aria-expanded={drawer()} onClick={toggleMetrics}>
          Metrics
          <Icon d={drawer() ? DOWN : UP} />
        </button>
      </footer>
    </div>
  );
}
