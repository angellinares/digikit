// The desktop developer console: the file the app runs, the live panel, and the audio and CPU meters.
// `mise run workbench -- FILE` starts it on FILE; with "watching" on, a change to FILE on disk reloads it, so a
// module can be patched from the command line and heard here a second later.
import { Show, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js';
import LivePanel, { type LivePanelApi } from '@digi/workbench-core/ui/components/LivePanel.tsx';
import { panels } from '@digi/workbench-core/data/faceplates.ts';
import {
  type EmulatorRuntime,
  type FirmwareInfo,
  type NativeAudioStatus,
  type RuntimeSnapshot,
  type RuntimeUpdate,
} from '@digi/workbench-core/runtime/runtime.ts';
import { booting as startBoot, bootingScreen, bootingStatus, loadFailure, type Booting } from './boot-core';
import { bumpCardRefresh, cardRefresh } from './card-state';
import CardPanel from './CardPanel';
import {
  CHART,
  DOWN,
  FIRMWARE,
  FOLDER,
  Icon,
  MODULES,
  PAUSE,
  PLAY,
  RELOAD,
  REMOTE,
  RESTART,
  SPEAKER,
  UP,
  WARNING,
} from './icons';
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
import { createNativeRuntime } from './native-runtime';
import { createPrefs } from './prefs';
import { pgoWarning, type BuildInfo } from './pgo-warning';
import { createLogFollow, followSession } from './log-follow';
import { createDkProfLoader } from './dkprof-env';
import { createDeviceNames, createEmulatorNames } from './log-names-backend';
import { windowShown } from './log-follow-core';
import { followOn, logEvent, logInput, logOpen, logState, setLogOpen } from './log-state';
import LogPanel from './LogPanel';
import type { EmuSource as Source, Host } from '@digi/workbench-core/host/host.ts';
import SidePanels from './SidePanels';
import TitleBar from './TitleBar';
import { setInspectorOpen, setInspectorTab } from './panel-open';
import RemotePage from './RemotePage';
import { useFirmwareApp, useTransferApp } from '@digi/workbench-core/transfer/app-context.ts';
import { EMULATOR_NO_RUNTIME } from '@digi/workbench-core/transfer/target.ts';
import { setReloadWithRuntime } from './emulator-actions';
import { running as flashRunning } from '@digi/workbench-core/firmware/firmware-core.ts';
import FirmwarePage from '@digi/workbench-core/ui/firmware/FirmwarePage.tsx';
import DefaultModules from '@digi/workbench-core/ui/firmware/DefaultModules.tsx';
import { DefaultModuleError } from '@digi/workbench-core/firmware/default-modules-core.ts';
import TransferPage from '@digi/workbench-core/ui/transfer/TransferPage.tsx';
import FilesPage from '@digi/workbench-core/ui/files/FilesPage.tsx';
import { cardLine, folderFiles, startCard, type CardStatus } from '@digi/workbench-core/runtime/cards.ts';
import { reloadError, reloadLine } from '@digi/workbench-core/runtime/reload.ts';
import type { ControllerJob } from '@digi/workbench-core/jobs/jobs-core.ts';
import { isCancelled } from '@digi/workbench-core/modules/backend.ts';
import { JobBusyError, type JobHandle } from '@digi/workbench-core/jobs/jobs.ts';

type Status = RuntimeSnapshot['status'];

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

// "Boot with digikit runtime": on unless the user turned it off; kept per browser profile of the app.
const RUNTIME_KEY = 'digikit.bootRuntime';

// "Load on start": boot the last opened firmware file when the app opens (off unless ticked), and that file's path.
const LOAD_ON_START_KEY = 'digikit.loadOnStart';
const LAST_FIRMWARE_KEY = 'digikit.lastFirmware';

// The height of the Log dock in px; kept per browser profile of the app (none saved: a share of the window).
const DOCK_KEY = 'digikit.logHeight';

// The sidebar is open unless the user collapsed it; kept per browser profile of the app.
const SIDEBAR_KEY = 'digikit.sidebar';

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

export default function Console(props: { host: Host }) {
  const app = useTransferApp();
  const fw = useFirmwareApp();
  const { readPref, writePref } = createPrefs(app.storage);
  const readRuntimePref = () => readPref(RUNTIME_KEY) !== 'off';
  const writeRuntimePref = (on: boolean) => writePref(RUNTIME_KEY, on ? 'on' : 'off');
  const readDockPref = () => parseDock(readPref(DOCK_KEY));
  const writeDockPref = (px: number | undefined) => writePref(DOCK_KEY, px === undefined ? null : String(px));
  const readSidebarPref = () => readPref(SIDEBAR_KEY) !== 'closed';
  const writeSidebarPref = (open: boolean) => writePref(SIDEBAR_KEY, open ? 'open' : 'closed');
  const { emulatorRuntime, emulatorSession, lastMidiPort, lastMidiSession, setEmulatorRuntime, target } = app.target;
  const { noteConnection } = app.connection;
  const { busy, currentJob, jobs, runJob, syncControllerJob } = app.jobs;
  const { setBrowseDevice } = app.state;
  const { tickedFor } = fw.defaults;
  const flashRun = fw.state.run;
  const [runtime, setRuntime] = createSignal<EmulatorRuntime>();
  const [controllerJob, setControllerJob] = createSignal<ControllerJob | null>(null);
  const [outsideApp, setOutsideApp] = createSignal(false);
  const [status, setStatus] = createSignal<Status>();
  const [error, setError] = createSignal('');
  const [paused, setPaused] = createSignal(false);
  const [source, setSource] = createSignal<Source>();
  /** A firmware is starting: the old device information is gone until its first status. */
  const [booting, setBooting] = createSignal<Booting>();
  /** The last load failed: its file, so the faceplate is the file's device, or none when the name does not tell. */
  /** The device that was showing: a file whose name does not tell its device keeps that faceplate. */
  const [lastDevice, setLastDevice] = createSignal<string>();
  const [failed, setFailed] = createSignal<Booting>();
  const [watching, setWatching] = createSignal(true);
  const [reloadNote, setReloadNote] = createSignal('');
  const [now, setNow] = createSignal(Date.now());
  const [audio, setAudio] = createSignal<NativeAudioStatus>();
  const [buildInfo, setBuildInfo] = createSignal<BuildInfo>();
  const [rate, setRate] = createSignal<number>();
  // the Log drawer: the last entry seen (the badge counts the errors after it); the filter and the list are in LogPanel.tsx
  const [seenId, setSeenId] = createSignal(0);
  // the Log dock: its height (undefined: the default share of the window), the window height, and a drag in progress
  const [dockSaved, setDockSaved] = createSignal(readDockPref());
  const [windowPx, setWindowPx] = createSignal(window.innerHeight);
  const [dockDrag, setDockDrag] = createSignal(false);
  const [page, setPage] = createSignal<'emulator' | 'transfer' | 'files' | 'remote' | 'firmware'>('emulator');
  const [sidebarOpen, setSidebarOpen] = createSignal(readSidebarPref());
  const [card, setCard] = createSignal<CardStatus>();
  const [bootRuntime, setBootRuntime] = createSignal(readRuntimePref());
  const [loadOnStart, setLoadOnStart] = createSignal(readPref(LOAD_ON_START_KEY) === 'on');
  const [info, setInfo] = createSignal<FirmwareInfo>();
  let live: LivePanelApi | undefined;
  let folderInput: HTMLInputElement | undefined;
  let lastAudio: { produced: number; wall: number } | undefined;
  let lastModified: number | undefined;
  let health = healthStart();
  let dockEl: HTMLElement | undefined;
  const timers: number[] = [];

  // a memo: `status` is set once per display frame, and the panels must hear of a change of this value only
  const sidePanelsShown = createMemo(() => page() === 'emulator');
  const panelsActive = createMemo(() => !!status());
  const device = () => {
    const d = status()?.device ?? booting()?.device ?? failed()?.device ?? lastDevice();
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
    if (target().kind === 'emulator' && lastDevice() !== event.snapshot.status.device)
      setBrowseDevice(event.snapshot.status.device === 'dn2' ? 'dn2' : 'dt2');
    setLastDevice(event.snapshot.status.device);
    setError(event.snapshot.status.error ?? '');
    watchHealth({ error: event.snapshot.status.error });
  };
  /** Log a guest fault, stall or DSP halt when it starts (the app polls the status; it gets no events from the controller). */
  const watchHealth = (reading: Parameters<typeof healthChanges>[1]) => {
    const seen = healthChanges(health, reading);
    health = seen.health;
    seen.entries.forEach(logInput);
  };

  // The emulator has the runtime when the image has a runtime record (patched now, from the cache, or a patched file loaded as it is);
  // unknown while booting.
  createEffect(() => {
    const i = info();
    setEmulatorRuntime(i === undefined ? 'unknown' : i.runtime ? 'yes' : 'no');
  });
  /** The tick differs from what the running image has: it applies at the next Reload or Open. Tick on: pending when the image has no
   * runtime. Tick off: pending when this boot patched the runtime in (a file loaded as it is, with its own runtime, keeps it either way). */
  const runtimePending = () => {
    const i = info();
    if (i === undefined) return false;
    return bootRuntime() ? !i.runtime : !!i.runtime?.patched;
  };
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
  const installDefaultModules = async (deviceId: string): Promise<void> => {
    try {
      const stop = new AbortController();
      await runJob(
        'Installing default modules',
        {
          group: 'emulator',
          cancel: () => {
            stop.abort();
            void runtime()?.cancelJob?.();
          },
        },
        (job) => performDefaultInstall(deviceId, job, stop.signal),
      );
    } catch (e) {
      if (!(e instanceof JobBusyError)) setError(message(e));
    }
  };
  const performDefaultInstall = async (deviceId: string, job: JobHandle, signal: AbortSignal) => {
    const ids = tickedFor(deviceId);
    if (ids.length === 0) return;
    try {
      const lines = await fw.env.defaultModules!.install(
        'emulator',
        deviceId,
        ids,
        (p) => job.progress(undefined, p.text),
        { signal, stillBooting: () => controllerJob() !== null },
      );
      setReloadNote(`Default modules ready: ${ids.join(', ')}`);
      logInput({
        source: 'firmware',
        level: 'info',
        text: `default modules in the emulator: ${ids.join(', ')}`,
        detail: lines.join('\n'),
      });
    } catch (e) {
      if (isCancelled(e) || isCancelled((e as { cause?: unknown })?.cause)) {
        logInput({ source: 'firmware', level: 'info', text: 'default modules cancelled' });
        bumpCardRefresh();
        return;
      }
      const what = e instanceof DefaultModuleError ? `default module ${e.id}` : 'default modules';
      setReloadNote(`${what} failed`);
      logInput(failureEntry('firmware', what, e instanceof DefaultModuleError ? e.message : message(e)));
    }
    bumpCardRefresh();
  };
  /** Start a new firmware session with `start` (every load goes through the controller's `firmware.load`). */
  const boot = async (
    start: (r: EmulatorRuntime) => Promise<RuntimeSnapshot>,
    name: string,
    withRuntime = bootRuntime(),
  ) => {
    const r = runtime();
    if (!r) return;
    live?.releaseAll();
    setError('');
    setStatus(undefined);
    setInfo(undefined);
    // a boot or reset of the emulator: the indicator asks again
    if (target().kind === 'emulator') noteConnection({ type: 'target' });
    setReloadNote('');
    setBooting(startBoot(name));
    logInput({ source: 'firmware', level: 'info', text: bootingScreen(startBoot(name)) });
    setFailed(undefined);
    health = healthStart();
    const began = performance.now();
    try {
      update({ type: 'snapshot', snapshot: await start(r) });
      setPaused(false);
      await refreshInfo();
      if (target().kind === 'emulator' && emulatorRuntime() === 'no')
        noteConnection({ type: 'noruntime', reason: EMULATOR_NO_RUNTIME });
      const s = status();
      if (s)
        logInput(
          firmwareEntry({
            name,
            device: s.device,
            version: s.version,
            runtime: withRuntime,
            patch: info()?.runtime,
            ms: performance.now() - began,
          }),
        );
      // another firmware may be another device: its card folder and its memory (the runtime needs a moment to load its modules)
      bumpCardRefresh();
      window.setTimeout(bumpCardRefresh, 2500);
      return true;
    } catch (e) {
      if (isCancelled(e) || isCancelled((e as { cause?: unknown })?.cause)) {
        setReloadNote(`Load cancelled: ${name}`);
        logInput({ source: 'firmware', level: 'info', text: `loading ${name} cancelled` });
        if (status()) await refreshInfo();
        return false;
      }
      setError(message(e));
      setFailed(startBoot(name));
      setReloadNote(loadFailure(name, message(e)));
      logInput(failureEntry('firmware', `loading ${name}`, message(e)));
      if (status()) await refreshInfo(); // A refused load can restore the previous live session.
      return false;
    } finally {
      setBooting(undefined);
    }
  };
  /** Make `path` the console's firmware file and boot it; the controller reads it from its path. */
  const bootPath = async (path: string, why: string, options: { runtime?: boolean; card?: string } = {}) => {
    const withRuntime = options.runtime ?? bootRuntime();
    const base = path.split(/[\\/]/).pop() || path;
    try {
      const loaded = await runJob(
        `Loading ${base}`,
        { group: 'emulator', cancel: () => void runtime()?.cancelJob?.() },
        async () => {
          const file = await props.host.emu.setSource(path);
          setSource(file ?? undefined);
          lastModified = file?.modified_ms;
          const ok = await boot(
            (r) =>
              r.loadPath
                ? r.loadPath(path, { runtime: withRuntime, card: options.card })
                : Promise.reject(new Error('no reload')),
            file?.name ?? path,
            withRuntime,
          );
          if (ok) {
            writePref(LAST_FIRMWARE_KEY, path);
            setReloadNote(`${why} ${new Date().toLocaleTimeString()}`);
          }
          return ok;
        },
      );
      // the install is its own job in the same group: it starts after the load job ended
      const s = status();
      if (loaded && s && withRuntime) void installDefaultModules(s.device);
    } catch (e) {
      if (e instanceof JobBusyError) return;
      setError(message(e));
    }
  };
  /** Boot the console's firmware file again. */
  const reload = async (why: string) => {
    const path = source()?.path;
    if (path) await bootPath(path, why);
  };
  /** The native picker; the picked file becomes the console's file and boots. */
  const openFirmware = async () => {
    const path = await props.host.emu.pickSyx(source()?.path ?? readPref(LAST_FIRMWARE_KEY));
    if (path) await bootPath(path, 'Opened at');
  };
  /** `dev.reload {build: true}`: the controller builds the stale ColdFire objects, applies the modules again from the objects on disk
   * and swaps module code into a restored main screen, or boots the new image; the result goes to the status area and the Log. A build
   * error is one short line in the status area; the Log has it with the compiler output as its detail. */
  const reloadModules = async () => {
    const r = runtime();
    if (!r?.reloadModules) return;
    try {
      await runJob('Reloading modules', { group: 'emulator' }, async () => {
        setError('');
        setReloadNote('Reloading modules…');
        try {
          const report = await r.reloadModules!();
          setReloadNote(reloadLine(report));
          logInput(reloadEntry(report));
          health = healthStart();
          setPaused(false);
          await refreshInfo();
        } catch (e) {
          if (isCancelled(e)) {
            setReloadNote('Module reload cancelled');
            logInput({ source: 'firmware', level: 'info', text: 'module reload cancelled' });
            return;
          }
          const failure = reloadError(e);
          setReloadNote(failure.status);
          logInput(reloadFailureEntry(failure));
        }
      });
    } catch (e) {
      if (!(e instanceof JobBusyError)) setError(message(e));
    }
  };
  // the "Boot with runtime" tick of other pages' "Reload with runtime" button
  setReloadWithRuntime(() => {
    setBootRuntime(true);
    writeRuntimePref(true);
    void reload('Reloaded with the runtime at');
  });
  const pollSource = async () => {
    try {
      const file = await props.host.emu.sourceInfo();
      setSource(file ?? undefined);
      if (!file) return;
      const changed = lastModified !== undefined && file.modified_ms !== lastModified;
      lastModified = file.modified_ms;
      if (changed && watching()) await reload('Reloaded after a change at');
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
    setNow(Date.now());
    try {
      const s = await runtime()?.controllerStatus?.();
      if (s?.heartbeat) watchHealth({ stalled: s.heartbeat.stalled, stallMs: s.heartbeat.stall_ms });
      setControllerJob(s?.job ?? null);
      syncControllerJob(s?.job, () => void runtime()?.cancelJob?.());
    } catch {
      /* a replaced session has no status */
    }
  };

  // an error banner goes away by itself after 8 s, unless the message changed meanwhile
  createEffect(() => {
    const text = error();
    if (!text) return;
    const timer = window.setTimeout(() => setError((current) => (current === text ? '' : current)), 8000);
    onCleanup(() => window.clearTimeout(timer));
  });

  const toggleLog = () => setLogOpen(!logOpen());
  const showMetrics = () => {
    setPage('emulator');
    setInspectorTab('metrics');
    setInspectorOpen(true);
    requestAnimationFrame(() => document.getElementById('side-tab-metrics')?.focus());
  };
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
      if (busy('emulator')) return;
      if (e.shiftKey) void reloadModules();
      else void reload('Reloaded at');
    }
  };
  onMount(() => {
    void createNativeRuntime(props.host, update).then((r) => {
      if (!r) {
        setOutsideApp(true);
        return;
      }
      setRuntime(r);
      void props.host.emu
        .buildInfo<BuildInfo>()
        .then(setBuildInfo)
        .catch(() => setBuildInfo(undefined));
      void (async () => {
        const launch = await props.host.emu.launch();
        if (launch.loaded) {
          await pollSource();
          await refreshInfo();
          return;
        }
        // The app opens empty; a command-line file, else the last opened file when "Load on start" is ticked, boots now.
        const path = launch.syx ?? (loadOnStart() ? readPref(LAST_FIRMWARE_KEY) : null);
        if (path)
          await bootPath(path, 'Loaded at', {
            runtime: launch.runtime ?? undefined,
            card: launch.card ?? undefined,
          });
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
    // a boot or reset of the emulator: the indicator asks again
    if (target().kind === 'emulator') noteConnection({ type: 'target' });
    health = healthStart();
    const began = performance.now();
    try {
      const s = await runtime()?.restart();
      if (s) update({ type: 'snapshot', snapshot: s });
      setPaused(false);
      await refreshInfo();
      if (target().kind === 'emulator' && emulatorRuntime() === 'no')
        noteConnection({ type: 'noruntime', reason: EMULATOR_NO_RUNTIME });
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
    try {
      await runJob(
        'Loading the sample card',
        { group: 'emulator', cancel: () => void runtime()?.cancelJob?.() },
        async () => {
          await startCard(cards, files, (done, total) =>
            setCard({ state: 'working', message: `Sending samples (${done}/${total})`, fraction: 0 }),
          );
          while (true) {
            const s = await cards.status();
            setCard(s);
            if (s.state !== 'working') {
              logEvent('card', s.state === 'error' ? 'error' : 'info', cardLine(s));
              break;
            }
            await new Promise((r) => setTimeout(r, 400));
          }
        },
      );
    } catch (e) {
      if (e instanceof JobBusyError) return;
      if (isCancelled(e)) {
        setReloadNote('Sample card load cancelled');
        logEvent('card', 'info', 'sample folder: cancelled');
        return;
      }
      setCard({ state: 'error', message: message(e) });
      logInput(failureEntry('card', 'sample folder', message(e)));
    }
  };

  // the Log drawer
  const badge = () => (logOpen() ? 0 : unseenErrors(logState().entries, seenId()));
  const mac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
  const logKey = mac ? '⌘J' : 'Ctrl+J';
  const reloadKey = mac ? '⌘R' : 'Ctrl+R';
  const modulesKey = mac ? '⇧⌘R' : 'Ctrl+Shift+R';
  // an open drawer has seen everything logged so far
  createEffect(() => {
    const entries = logState().entries;
    if (logOpen()) setSeenId(entries.at(-1)?.id ?? 0);
  });
  // the runtime logs of the emulator and of the device on the MIDI port chosen last (nothing is read while the emulator is paused)
  const dkprof = createDkProfLoader(props.host);
  const logFollow = createLogFollow({
    follow: followOn,
    flashing: () => flashRunning(flashRun()),
    shown: windowShown,
    drawerOpen: logOpen,
    emulator: {
      running: () => !!status() && !paused(),
      session: async () => followSession(await emulatorSession()),
      names: createEmulatorNames(props.host, dkprof),
    },
    device: {
      port: lastMidiPort,
      session: async () => followSession(await lastMidiSession()),
      present: async () => (await app.env.targets.ports()).includes(lastMidiPort() ?? ''),
      names: createDeviceNames(props.host, dkprof),
    },
  });

  const fileLabel = () => source()?.path || 'Open firmware…';
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

  /** The Metrics tab of the right-hand column: what the emulator measures, polled once a second, and two test actions. */
  const metrics = (
    <div class="card-body metrics">
      <p class="poll">1 s poll</p>
      <Show when={!status()}>
        <p class="note">The emulator is not running: no readings yet.</p>
      </Show>
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
      <Show when={device() === 'dt2' && runtime()?.cards}>
        <div class="acts">
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
            disabled={!status() || busy('emulator')}
            title="Pick a plus_drive-style folder: WAVs become samples (track N plays file N, sorted by name), .mmcfs/ and digikit/ are used as they are"
            onClick={() => folderInput?.click()}
          >
            Sample folder…
          </button>
        </div>
      </Show>
    </div>
  );

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
      <TitleBar onError={(m) => setError(m)} />

      <Show when={error() || outsideApp()}>
        <p class="alert" role="alert">
          <span>
            {outsideApp() ? 'This page is the desktop app’s console; start it with mise run workbench.' : error()}
          </span>
          <Show when={error()}>
            <button type="button" class="dismiss" aria-label="Dismiss" onClick={() => setError('')}>
              ×
            </button>
          </Show>
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
          <span class="side-group">Run</span>
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
          <span class="side-group">Device</span>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'transfer' }}
            aria-current={page() === 'transfer' ? 'page' : undefined}
            title="Choose the modules that run on the target (the emulator or a device on a MIDI port) and install them"
            onClick={() => setPage('transfer')}
          >
            <Icon d={MODULES} />
            <span>Transfer</span>
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'files' }}
            aria-current={page() === 'files' ? 'page' : undefined}
            title="See every file in the target's card store: space, sizes and module headers"
            onClick={() => setPage('files')}
          >
            <Icon d={FOLDER} />
            <span>Files</span>
          </button>
          <button
            type="button"
            class="side"
            classList={{ on: page() === 'remote' }}
            aria-current={page() === 'remote' ? 'page' : undefined}
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
          <div class="emu-page" hidden={page() !== 'emulator'}>
            <header class="page-head emu-head">
              <h2>Emulator</h2>
              <span class="file-chip" title={fileLabel()}>
                <button
                  type="button"
                  class="file-open"
                  disabled={busy('emulator')}
                  aria-label="Open firmware file"
                  title={source() ? 'Open another firmware file' : 'Open a firmware file'}
                  onClick={() => void openFirmware()}
                >
                  <Icon d={FOLDER} />
                  <span class="path">{shortPath()}</span>
                </button>
                <Show when={status()}>
                  <span class="tag device">
                    {status()!.device} {status()!.version}
                  </span>
                </Show>
                <Show when={source()}>
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
              </span>
              <span class="sp" />
              <label
                class="tag opt"
                title="Boot this firmware file when the app opens (a file named on the command line wins)"
              >
                <input
                  type="checkbox"
                  checked={loadOnStart()}
                  onChange={(e) => {
                    setLoadOnStart(e.currentTarget.checked);
                    writePref(LOAD_ON_START_KEY, e.currentTarget.checked ? 'on' : 'off');
                  }}
                />
                <span class="long">Load on start</span>
                <span class="short">On start</span>
              </label>
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
                <span class="long">Boot with runtime</span>
                <span class="short">Runtime</span>
                <Show when={info()}>
                  {(i) => (
                    <span
                      class="rt"
                      classList={{ on: !!i().runtime?.patched, pending: runtimePending() }}
                      title={
                        runtimePending()
                          ? `The running image ${i().runtime ? 'has' : 'has no'} runtime; the tick applies at the next Reload (${reloadKey}) or Open`
                          : i().runtime?.patched
                            ? `${i().runtime?.modules?.length ?? 0} modules of the "${i().runtime?.set}" set, ${i().runtime?.cache_hit ? 'from the image cache' : 'patched now'}`
                            : i().runtime
                              ? 'The image has a patch record: it is loaded as it is'
                              : 'The image is loaded as it is'
                      }
                    >
                      {runtimePending()
                        ? 'on next Reload'
                        : i().runtime?.patched
                          ? `${i().runtime?.set}${i().runtime?.cache_hit ? ' (cached)' : ''}`
                          : i().runtime
                            ? 'patched image'
                            : 'stock'}
                    </span>
                  )}
                </Show>
              </label>
              <span class="tb-group">
                <DefaultModules
                  device={device()}
                  off={bootRuntime() ? '' : 'Boot with digikit runtime is off: no module can be installed'}
                  note="Uploaded and enabled in the emulator after the firmware loads."
                />
              </span>
              <span class="tb-group">
                <button
                  type="button"
                  class="tb"
                  classList={{ pending: runtimePending() }}
                  disabled={!source() || busy('emulator')}
                  aria-label="Reload"
                  title={`Reload the startup file (${reloadKey})${runtimePending() ? ' · applies the Runtime choice' : ''}`}
                  onClick={() => void reload('Reloaded at')}
                >
                  <Icon d={RELOAD} />
                  <span class="lbl">Reload</span>
                  <kbd class="lbl">{reloadKey}</kbd>
                </button>
                <button
                  type="button"
                  class="tb"
                  disabled={!status() || busy('emulator')}
                  aria-label="Reload modules"
                  title={`Build the stale module objects, then apply the modules into the running session, without a boot when only UI modules changed (${modulesKey}). A changed SHARC+ source still needs mise run dk:dev.`}
                  onClick={() => void reloadModules()}
                >
                  <Icon d={MODULES} />
                  <span class="lbl">Modules</span>
                  <kbd class="lbl">{modulesKey}</kbd>
                </button>
              </span>
              <span class="tb-group">
                <button
                  type="button"
                  class="tb icon"
                  disabled={!status()}
                  aria-label={paused() ? 'Resume' : 'Pause'}
                  title={paused() ? 'Resume the emulator' : 'Pause the emulator'}
                  onClick={togglePause}
                >
                  <Icon d={paused() ? PLAY : PAUSE} />
                </button>
                <button
                  type="button"
                  class="tb icon"
                  disabled={!status()}
                  aria-label="Reset (emulator)"
                  title="Reset the running firmware to its ready state (the emulator's reset; not the device's RESTART command)"
                  onClick={() => void restart()}
                >
                  <Icon d={RESTART} />
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
              </span>
            </header>
            <div class="emu-row">
              <main class="stage" aria-label={`${panel().name} front panel`}>
                <div class="surface">
                  <LivePanel
                    panel={panel()}
                    runtime={runtime}
                    onError={(e) => setError(e)}
                    ref={(api) => (live = api)}
                  />
                </div>
              </main>
              <SidePanels
                host={props.host}
                visible={sidePanelsShown()}
                active={panelsActive()}
                metrics={metrics}
                session={emulatorSession}
                where={() => 'the emulator'}
              />
            </div>
          </div>
          <Show when={page() === 'transfer'}>
            <TransferPage onFlash={() => setPage('firmware')} />
          </Show>
          <Show when={page() === 'files'}>
            <FilesPage
              refresh={cardRefresh}
              halted={() =>
                target().kind === 'emulator' && paused() ? 'The emulator is paused. Resume it to read its card.' : null
              }
              save={(name, bytes) => props.host.fs.saveLog(bytes, name, 'file')}
              extra={
                <Show when={target().kind === 'emulator'}>
                  <CardPanel host={props.host} reload={restart} />
                </Show>
              }
            />
          </Show>
          <Show when={page() === 'remote'}>
            <RemotePage fallbackDevice={device()} emulatorLoaded={!!status()} host={props.host} />
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
            host={props.host}
            follow={logFollow}
            emulatorRunning={() => !!status()}
            devicePort={lastMidiPort}
            toggleKey={logKey}
          />
        </section>
      </Show>

      <footer class="statusbar">
        <span>
          <i class="dot" classList={{ paused: paused() || !status() || !!booting() }} />
          {booting()
            ? bootingStatus(booting()!)
            : !status()
              ? 'no firmware'
              : paused()
                ? 'paused'
                : phase() || 'running'}
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
            <span title="Estimated DSP load of the real chip. Model figure (as the HEALTH page shows in the emulator) and window peak are in the Metrics tab">
              DSP est. {Math.round(load_()!.dsp!.real_pct)}%
              <Meter value={load_()!.dsp!.real_pct} worst={load_()!.dsp!.real_high_pct} />
            </span>
          </Show>
        </Show>
        <span class="sp" />
        <Show
          when={currentJob()}
          fallback={
            <Show when={reloadNote()}>
              <span class="muted" title={reloadNote()}>
                {reloadNote()}
              </span>
            </Show>
          }
        >
          {(job) => (
            <span class="job" title={job().detail ? `${job().label}: ${job().detail}` : job().label}>
              <Icon d={RELOAD} />
              <span class="label">{job().label}</span>
              <Show when={job().detail}>
                <span class="detail">{job().detail}</span>
              </Show>
              <span>{Math.max(0, Math.round((now() - job().started) / 1000))} s</span>
              <Show when={job().progress !== undefined}>
                <Meter value={job().progress! * 100} tone="amber" />
                <span>~{Math.round(job().progress! * 100)}%</span>
              </Show>
              <Show when={jobs().list.length > 1}>
                <span class="more">+{jobs().list.length - 1}</span>
              </Show>
              <Show when={job().cancel}>
                <button type="button" class="cancel" onClick={() => job().cancel?.()}>
                  Cancel
                </button>
              </Show>
            </span>
          )}
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
        <button type="button" class="sb" title="Show the Metrics tab of the Emulator page" onClick={showMetrics}>
          Metrics
        </button>
      </footer>
    </div>
  );
}
