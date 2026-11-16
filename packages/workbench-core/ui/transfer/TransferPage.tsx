// The Transfer page, shared by the desktop workbench and the site (the app gives its parts in env.ts): one page, up to two modes. A dropdown
// picks Modules (the built modules of the library and the card's module store) or, in the desktop app, Samples (a folder of samples and the
// device's sample folders). The left column is always "On your computer", the right
// column "On device"; each column carries its own controls (ModulesMode.tsx, SamplesMode.tsx; the shared parts are in TransferParts.tsx).
// This file holds what the modes share: the top bar (title, mode), the one busy action, the error line, and the progress bar along the bottom.
// There is no Refresh button: the page reads by itself (AutoRefresh.tsx, refresh-core.ts). The page speaks the upload protocol itself, in
// TypeScript, over the controller's USB capture; no process is started and nothing is built.
import { Match, Show, Switch, batch, createEffect, createMemo, createSignal, on, onMount, type JSX } from 'solid-js';
import { Dynamic } from 'solid-js/web';
import Segmented from '../components/Segmented';
import { isCancelled, isNoRuntime, isTimeout } from '../../src/modules/backend';
import { noteActivity } from '../../src/transfer/activity';
import AutoRefresh from './AutoRefresh';
import ConnectionBadge from './ConnectionBadge';
import type { TransferApp } from '../../src/transfer/app';
import { useTransferApp } from '../../src/transfer/app-context';
import ModulesMode from './ModulesMode';
import SourcePanel from './SourcePanel';
import TransferBar from './TransferBar';
import { MODES, modulesBar, parseMode, rowKey, storageLine, type TransferView } from '../../src/transfer/transfer-core';
import { targetLabel } from '../../src/transfer/target';
import { writing, type CancelledRun, type PageCtx } from './TransferParts';
import { failureLine } from '../../src/transfer/wait-core';
import './transfer-page.css';

/** Tick the modules named in the page URL (`?add=`) once the list has them; a row's id is the full module id (`<author>/<name>`). */
function applyPendingAdd(app: TransferApp, view: TransferView): void {
  const { pendingAdd, setTicks, setPendingAdd } = app.state;
  const pending = pendingAdd();
  if (!pending.length) return;
  const keys = view.modules.filter((r) => r.id && pending.some((p) => p === r.id)).map(rowKey);
  setTicks((t) => new Set([...t, ...keys]));
  setPendingAdd([]);
}

/** Read what the mode shows, both sides: the device and the computer (the library, or the chosen samples folder). */
async function read(app: TransferApp): Promise<void> {
  const { env } = app;
  const { target } = app.target;
  const { activeMode, browseDevice, browsing, lastView, setBrowsing, setBrowseDevice, setDeviceInfo, setLastView } =
    app.state;
  if (activeMode() === 'samples' && env.extras?.samples) return env.extras.samples.read();
  const readingTarget = target();
  const current = () => target() === readingTarget && activeMode() === 'modules';
  let local: TransferView | undefined;
  const showLocal = (library: TransferView) => {
    setBrowsing(true);
    setLastView(library);
    setDeviceInfo(undefined);
    applyPendingAdd(app, library);
  };
  let v: TransferView;
  try {
    v = await app.backend.list(browseDevice(), (library) => {
      local = library;
      // Keep the previous device rows during a refresh; only a first read needs the library-only view.
      if (current() && (!lastView() || browsing())) showLocal(library);
    });
  } catch (e) {
    if (current() && local) showLocal(local);
    throw e;
  }
  if (!current()) return;
  setBrowsing(false);
  setBrowseDevice(v.device === 'dn2' ? 'dn2' : 'dt2');
  setLastView(v);
  applyPendingAdd(app, v);
  const name = await env.deviceName(v.device);
  if (current()) setDeviceInfo({ name, runtime: v.runtime, storage: storageLine(v) });
}

const haltOn = (app: TransferApp, e: unknown): void => {
  app.state.setHalted(isTimeout(e) || isNoRuntime(e));
  app.state.setNoRuntime(isNoRuntime(e));
};

/** The one busy action. It is not made by the page: a job goes on when the page is left, and the old and the new page share it. */
const run = async (app: TransferApp, what: string, f: () => Promise<void | CancelledRun>) => {
  const { target } = app.target;
  const { activeMode, beginJob, endJob, error, log, progress, setError, setHalted, setNoRuntime } = app.state;
  if (!beginJob(what, cancellable(what) ? () => cancelJob(app) : undefined)) return;
  noteActivity();
  setError('');
  // after a cancel or a "no reply" the device is not asked again at once (that would wait the whole time a second time)
  let skipRead = false;
  // a read is not logged (the page reads by itself); every other run is, with its result
  const logRun = (failure?: unknown, cancelled?: string) => {
    if (what === 'Reading') return;
    const modules = activeMode() === 'modules';
    app.env.logRun({
      what,
      target: targetLabel(target()),
      lines: modules ? log() : (app.env.extras?.samples?.lines?.() ?? []),
      message: modules ? progress()?.message : undefined,
      ...(failure === undefined
        ? cancelled === undefined
          ? {}
          : { cancelled: true, counts: cancelled }
        : { error: failureLine(failure, targetLabel(target())), cancelled: isCancelled(failure) }),
    });
  };
  try {
    // a job that the user stopped is not a failure: no error line, the log says so, and both sides are read again
    const result = await f();
    setHalted(false);
    setNoRuntime(false);
    logRun(undefined, result ? result.cancelled : undefined);
  } catch (e) {
    skipRead = isCancelled(e) || isTimeout(e) || isNoRuntime(e);
    haltOn(app, e);
    setError(failureLine(e, targetLabel(target())));
    logRun(e);
  } finally {
    if (!skipRead)
      try {
        await read(app);
      } catch (e) {
        haltOn(app, e);
        if (!error()) setError(failureLine(e, targetLabel(target())));
      }
    if (endJob()) void run(app, 'Reading', async () => {});
    app.env.extras?.onJobEnd?.();
  }
};

/** The jobs whose Cancel is safe: a file is atomic on the card (the store changes at END only, and a cut-off upload is ABORTed), so a cancel
 * leaves the files that were verified and none half done. Delete and Restart are short and have nothing to stop in the middle. */
const cancellable = (what: string): boolean => what === 'Uploading' || what === 'Installing';

/** Stop the running job: no later file starts, and every request that waits on the session ends as cancelled. */
const cancelJob = (app: TransferApp): void => {
  app.state.askStop();
  app.target.cancelPending();
};

/** `onFlash`: starts the one-click Flash flow from a service refusal (the host gives it: the desktop opens its Firmware page, the site links to its own). */
export default function TransferPage(props: { onFlash?: () => void }): JSX.Element {
  const app = useTransferApp();
  const { env } = app;
  const { target, emulatorRuntime, rescanTargets } = app.target;
  const { connection } = app.connection;
  const {
    activeMode,
    browsing,
    busy,
    error,
    halted,
    jobProgress,
    log,
    noRuntime,
    progress,
    setDelTicks,
    setDeviceInfo,
    setError,
    setHalted,
    setLastView,
    setMode,
    setTicks,
  } = app.state;
  const [reload, setReload] = createSignal(0);

  const ctx: PageCtx = { busy, run: (what, f) => run(app, what, f), setError, reload };
  const readNow = () => {
    setHalted(false);
    void run(app, 'Reading', async () => {});
  };
  const autoRead = () => {
    if (!halted()) readNow();
  };
  // a change of the emulator's runtime, or a reconnect, lets a halted page read again
  createEffect(
    on(
      emulatorRuntime,
      () => {
        if (halted() && target().kind === 'emulator') readNow();
      },
      { defer: true },
    ),
  );
  createEffect(
    on(
      () => connection().state,
      (s) => {
        if (s === 'connected' && halted()) readNow();
      },
      { defer: true },
    ),
  );

  // back on the page during a job: that job's end reads; a read now would only wait behind it
  onMount(() => {
    if (!busy()) void run(app, 'Reading', async () => {});
  });
  // another target: what was read from the old one is not shown; read the new one
  createEffect(
    on(
      target,
      () => {
        setHalted(false);
        setLastView(undefined);
        setDeviceInfo(undefined);
        // ticks made before a device was plugged in survive (the list shows the library alone then)
        if (!browsing()) setTicks(new Set<string>());
        setDelTicks(new Set<string>());
        env.extras?.samples?.reset();
        void run(app, 'Reading', async () => {});
      },
      { defer: true },
    ),
  );
  // another mode: the device line is the mode's own (its free space), so read it again
  const changeMode = (value: string) => {
    setMode(parseMode(value));
    setError('');
    setDeviceInfo(undefined);
    void run(app, 'Reading', async () => {});
  };

  // the bar along the bottom, for the install, delete or restart of Modules and the upload of Samples
  const bar = createMemo(
    () => {
      if (activeMode() === 'modules') {
        const p = progress();
        return p ? { info: modulesBar(p), running: writing(busy()) } : undefined;
      }
      return env.extras?.samples?.bar(busy());
    },
    undefined,
    {
      // a builder may return a fresh object each time: equal fields must not notify
      equals: (a, b) =>
        a === b ||
        (!!a &&
          !!b &&
          a.running === b.running &&
          a.info.fraction === b.info.fraction &&
          a.info.message === b.info.message &&
          a.info.failed === b.info.failed &&
          a.info.sent === b.info.sent &&
          a.info.total === b.info.total &&
          a.info.checking === b.info.checking),
    },
  );
  const samples = () => env.extras?.samples;
  // the status bar shows the same fraction and line
  createEffect(() => {
    const b = bar();
    if (b?.running) jobProgress(b.info.fraction, b.info.message);
  });

  return (
    <section class="transfer-page" aria-label="Transfer">
      <header class="head">
        <h2>Transfer</h2>
        <Show when={samples()}>
          <Segmented
            label="Mode"
            options={MODES}
            value={activeMode()}
            onChange={(m) => changeMode(m)}
            disabled={writing(busy())}
          />
        </Show>
        <span class="sp" />
        <ConnectionBadge />
        <Show when={busy() === 'Reading'}>
          <span class="meta">reading…</span>
        </Show>
      </header>
      <AutoRefresh
        busy={busy}
        read={autoRead}
        rescanPorts={() => {
          batch(() => {
            setReload((n) => n + 1);
            rescanTargets();
          });
        }}
      />

      <Show when={error()}>
        <p class="err" role="alert">
          <span class="msg">{error()}</span>
          <button type="button" class="act small" disabled={!!busy()} onClick={readNow}>
            Try again
          </button>
          <Show when={noRuntime() && target().kind === 'emulator' && env.extras?.reloadWithRuntime}>
            <button type="button" class="act small" onClick={() => env.extras?.reloadWithRuntime?.()}>
              Reload with runtime
            </button>
          </Show>
        </p>
      </Show>

      <Switch>
        <Match when={activeMode() === 'modules'}>
          <ModulesMode ctx={ctx} onFlash={props.onFlash} />
        </Match>
        <Match when={activeMode() === 'samples' && samples()}>
          {(slot) => <Dynamic component={slot().Mode} ctx={ctx} />}
        </Match>
      </Switch>

      <Show when={bar()}>
        {(b) => (
          <TransferBar
            bar={b().info}
            running={b().running}
            onCancel={cancellable(busy()) ? () => cancelJob(app) : undefined}
          />
        )}
      </Show>
      <Show when={activeMode() === 'modules' && log().length && !bar()}>
        <pre class="log">{log().join('\n')}</pre>
      </Show>
      <SourcePanel />
    </section>
  );
}
