// The Transfer page, shared by the desktop workbench and the site (the app gives its parts in env.ts): one page, up to two modes. A dropdown
// picks Modules (the built modules of the library and the card's module store) or, in the desktop app, Samples (a folder of samples and the
// device's sample folders). The left column is always "On your computer", the right
// column "On device"; each column carries its own controls (ModulesMode.tsx, SamplesMode.tsx; the shared parts are in TransferParts.tsx).
// This file holds what the modes share: the top bar (title, mode), the one busy action, the error line, and the progress bar along the bottom.
// There is no Refresh button: the page reads by itself (AutoRefresh.tsx, refresh-core.ts). The page speaks the upload protocol itself, in
// TypeScript, over the controller's USB capture; no process is started and nothing is built.
import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onMount, type JSX } from 'solid-js';
import { Dynamic } from 'solid-js/web';
import { isCancelled, isTimeout } from '../modules/backend';
import { noteActivity } from './activity';
import AutoRefresh from './AutoRefresh';
import ConnectionBadge from './ConnectionBadge';
import { transferEnv } from './env';
import ModulesMode from './ModulesMode';
import { target, targetLabel } from './target';
import SourcePanel from './SourcePanel';
import { transferBackend } from './transfer-backend';
import TransferBar from './TransferBar';
import { MODES, modulesBar, parseMode, storageLine } from './transfer-core';
import { writing, type PageCtx } from './TransferParts';
import {
  activeMode,
  log,
  progress,
  setDelTicks,
  setDeviceInfo,
  setLastView,
  setMode,
  setTicks,
} from './transfer-state';
import WaitNotice from './WaitNotice';
import { failureLine } from './wait-core';
import './transfer-page.css';

/** Read what the mode shows, both sides: the device and the computer (the library, or the chosen samples folder). */
async function read(): Promise<void> {
  const env = transferEnv();
  if (activeMode() === 'samples' && env.extras?.samples) return env.extras.samples.read();
  const v = await transferBackend.list();
  setLastView(v);
  setDeviceInfo({ name: await env.deviceName(v.device), runtime: v.runtime, storage: storageLine(v) });
}

export default function TransferPage(): JSX.Element {
  const [busy, setBusy] = createSignal('');
  const [error, setError] = createSignal('');
  const [reload, setReload] = createSignal(0);

  // a read asked for while another job runs (another target or mode during a wait) runs when that job ends
  let readAgain = false;
  const run = async (what: string, f: () => Promise<void>) => {
    if (busy()) {
      if (what === 'Reading') readAgain = true;
      return;
    }
    setBusy(what);
    noteActivity();
    setError('');
    // after a cancel or a "no reply" the device is not asked again at once (that would wait the whole time a second time)
    let skipRead = false;
    // a read is not logged (the page reads by itself); every other run is, with its result
    const logRun = (failure?: unknown) => {
      if (what === 'Reading') return;
      const modules = activeMode() === 'modules';
      transferEnv().logRun({
        what,
        target: targetLabel(target()),
        lines: modules ? log() : (transferEnv().extras?.samples?.lines?.() ?? []),
        message: modules ? progress()?.message : undefined,
        ...(failure === undefined
          ? {}
          : { error: failureLine(failure, targetLabel(target())), cancelled: isCancelled(failure) }),
      });
    };
    try {
      await f();
      logRun();
    } catch (e) {
      skipRead = isCancelled(e) || isTimeout(e);
      setError(failureLine(e, targetLabel(target())));
      logRun(e);
    } finally {
      if (!skipRead)
        try {
          await read();
        } catch (e) {
          if (!error()) setError(failureLine(e, targetLabel(target())));
        }
      setBusy('');
      if (readAgain) {
        readAgain = false;
        void run('Reading', async () => {});
      }
      transferEnv().extras?.onJobEnd?.();
    }
  };
  const ctx: PageCtx = { busy, run, setError, reload };
  const readNow = () => void run('Reading', async () => {});

  onMount(() => void run('Reading', async () => {}));
  // another target: what was read from the old one is not shown; read the new one
  createEffect(
    on(
      target,
      () => {
        setLastView(undefined);
        setDeviceInfo(undefined);
        setTicks(new Set<string>());
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
          <label class="target-select">
            <span class="dim">Mode</span>{' '}
            <select
              aria-label="Mode"
              disabled={writing(busy())}
              value={activeMode()}
              onChange={(e) => changeMode(e.currentTarget.value)}
            >
              <For each={MODES}>{(m) => <option value={m.id}>{m.label}</option>}</For>
            </select>
          </label>
        </Show>
        <span class="sp" />
        <ConnectionBadge />
        <Show when={busy() === 'Reading'}>
          <span class="meta">reading…</span>
        </Show>
      </header>
      <AutoRefresh busy={busy} read={readNow} rescanPorts={() => setReload((n) => n + 1)} />

      <WaitNotice active={!!busy()} />

      <Show when={error()}>
        <p class="err" role="alert">
          <span class="msg">{error()}</span>
          <button type="button" class="act small" disabled={!!busy()} onClick={readNow}>
            Try again
          </button>
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

      <Show when={bar()}>{(b) => <TransferBar bar={b().info} running={b().running} />}</Show>
      <Show when={activeMode() === 'modules' && log().length && !bar()}>
        <pre class="log">{log().join('\n')}</pre>
      </Show>
      <SourcePanel />
    </section>
  );
}
