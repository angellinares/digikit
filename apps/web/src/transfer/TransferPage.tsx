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
import { isCancelled, isNoRuntime, isTimeout } from '../modules/backend';
import { noteActivity } from './activity';
import AutoRefresh from './AutoRefresh';
import ConnectionBadge from './ConnectionBadge';
import { connection } from './connection-state';
import { transferEnv } from './env';
import { rescanTargets } from './TargetSelect';
import ModulesMode from './ModulesMode';
import { cancelPending, emulatorRuntime, target, targetLabel } from './target';
import SourcePanel from './SourcePanel';
import { transferBackend } from './transfer-backend';
import TransferBar from './TransferBar';
import { MODES, modulesBar, parseMode, rowKey, storageLine, type TransferView } from './transfer-core';
import { writing, type CancelledRun, type PageCtx } from './TransferParts';
import {
  activeMode,
  askStop,
  beginJob,
  browseDevice,
  browsing,
  busy,
  endJob,
  error,
  log,
  pendingAdd,
  progress,
  setBrowsing,
  setDelTicks,
  setDeviceInfo,
  setError,
  setLastView,
  setMode,
  setPendingAdd,
  setTicks,
} from './transfer-state';
import WaitNotice from './WaitNotice';
import { failureLine } from './wait-core';
import './transfer-page.css';

/** Tick the modules named in the page URL (`?add=`) once the list has them; a row's id is the full module id (`<author>/<name>`). */
function applyPendingAdd(view: TransferView): void {
  const pending = pendingAdd();
  if (!pending.length) return;
  const keys = view.modules.filter((r) => r.id && pending.some((p) => p === r.id)).map(rowKey);
  setTicks((t) => new Set([...t, ...keys]));
  setPendingAdd([]);
}

/** Read what the mode shows, both sides: the device and the computer (the library, or the chosen samples folder). */
async function read(): Promise<void> {
  const env = transferEnv();
  if (activeMode() === 'samples' && env.extras?.samples) return env.extras.samples.read();
  let v: TransferView;
  try {
    v = await transferBackend.list();
  } catch (e) {
    if (!env.libraryDevices) throw e;
    // no device answered: show the library and keep the reason on the error line
    const library = await transferBackend.listLibrary(browseDevice());
    setBrowsing(true);
    setLastView(library);
    setDeviceInfo(undefined);
    applyPendingAdd(library);
    throw e;
  }
  setBrowsing(false);
  setLastView(v);
  applyPendingAdd(v);
  setDeviceInfo({ name: await env.deviceName(v.device), runtime: v.runtime, storage: storageLine(v) });
}

// After "no reply" or "no runtime" the page does not read by itself; Try again, another target, a change of the emulator's runtime or a
// reconnect lets it read again.
const [halted, setHalted] = createSignal(false);
const [noRuntime, setNoRuntime] = createSignal(false);
const haltOn = (e: unknown): void => {
  setHalted(isTimeout(e) || isNoRuntime(e));
  setNoRuntime(isNoRuntime(e));
};

/** The one busy action. It is not made by the page: a job goes on when the page is left, and the old and the new page share it. */
const run = async (what: string, f: () => Promise<void | CancelledRun>) => {
  if (!beginJob(what)) return;
  noteActivity();
  setError('');
  // after a cancel or a "no reply" the device is not asked again at once (that would wait the whole time a second time)
  let skipRead = false;
  // a read is not logged (the page reads by itself); every other run is, with its result
  const logRun = (failure?: unknown, cancelled?: string) => {
    if (what === 'Reading') return;
    const modules = activeMode() === 'modules';
    transferEnv().logRun({
      what,
      target: targetLabel(target()),
      lines: modules ? log() : (transferEnv().extras?.samples?.lines?.() ?? []),
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
    haltOn(e);
    setError(failureLine(e, targetLabel(target())));
    logRun(e);
  } finally {
    if (!skipRead)
      try {
        await read();
      } catch (e) {
        haltOn(e);
        if (!error()) setError(failureLine(e, targetLabel(target())));
      }
    if (endJob()) void run('Reading', async () => {});
    transferEnv().extras?.onJobEnd?.();
  }
};

/** The jobs whose Cancel is safe: a file is atomic on the card (the store changes at END only, and a cut-off upload is ABORTed), so a cancel
 * leaves the files that were verified and none half done. Delete and Restart are short and have nothing to stop in the middle. */
const cancellable = (what: string): boolean => what === 'Uploading' || what === 'Installing';

/** Stop the running job: no later file starts, and every request that waits on the session ends as cancelled. */
const cancelJob = (): void => {
  askStop();
  cancelPending();
};

export default function TransferPage(): JSX.Element {
  const [reload, setReload] = createSignal(0);

  const ctx: PageCtx = { busy, run, setError, reload };
  const readNow = () => {
    setHalted(false);
    void run('Reading', async () => {});
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
    if (!busy()) void run('Reading', async () => {});
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
        transferEnv().extras?.samples?.reset();
        void run('Reading', async () => {});
      },
      { defer: true },
    ),
  );
  // another mode: the device line is the mode's own (its free space), so read it again
  const changeMode = (value: string) => {
    setMode(parseMode(value));
    setError('');
    setDeviceInfo(undefined);
    void run('Reading', async () => {});
  };

  // the bar along the bottom, for the install, delete or restart of Modules and the upload of Samples
  const bar = createMemo(() => {
    if (activeMode() === 'modules') {
      const p = progress();
      return p ? { info: modulesBar(p), running: writing(busy()) } : undefined;
    }
    return transferEnv().extras?.samples?.bar(busy());
  });
  const samples = () => transferEnv().extras?.samples;

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

      <div class="notices">
        <WaitNotice active={!!busy()} cancel={!cancellable(busy())} />
      </div>

      <Show when={error()}>
        <p class="err" role="alert">
          <span class="msg">{error()}</span>
          <button type="button" class="act small" disabled={!!busy()} onClick={readNow}>
            Try again
          </button>
          <Show when={noRuntime() && target().kind === 'emulator' && transferEnv().extras?.reloadWithRuntime}>
            <button type="button" class="act small" onClick={() => transferEnv().extras?.reloadWithRuntime?.()}>
              Reload with runtime
            </button>
          </Show>
        </p>
      </Show>

      <Switch>
        <Match when={activeMode() === 'modules'}>
          <ModulesMode ctx={ctx} />
        </Match>
        <Match when={activeMode() === 'samples' && samples()}>
          {(slot) => <Dynamic component={slot().Mode} ctx={ctx} />}
        </Match>
      </Switch>

      <Show when={bar()}>
        {(b) => (
          <TransferBar bar={b().info} running={b().running} onCancel={cancellable(busy()) ? cancelJob : undefined} />
        )}
      </Show>
      <Show when={activeMode() === 'modules' && log().length && !bar()}>
        <pre class="log">{log().join('\n')}</pre>
      </Show>
      <SourcePanel />
    </section>
  );
}
