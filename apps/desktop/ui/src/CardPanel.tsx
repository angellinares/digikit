// The card-folder strip of the Files page (emulator only): the host folder of the loaded device's card (`plus_drive/<device>/` or the folder
// the user chose for the device). The module store is mirrored to it (kept up to date by the emulator itself about 2 s after the guest's last
// card write; one way, card to folder) and the card of a load is built from it. It shows the device and the folder, the time of the last
// sync, "Choose folder...", "Use default", "Show in Finder", "Sync now" and "Reset card changes". A device whose card is built
// from the folder (the Digitakt II) takes a new folder at the next load: the strip says so and offers "Reload now". The emulator does the
// work and keeps the choice per device (`card.folder`, `card.folder_set`, `card.sync` through `emu_call`); this strip only asks.
import { Show, createEffect, createSignal, onCleanup, untrack, type JSX } from 'solid-js';
import { errorLine } from '@digi/workbench-core/modules/backend.ts';
import { panels } from '@digi/workbench-core/data/faceplates.ts';
import { canUseDefault, folderNotice, shortFolder, statusText, type CardFolder } from './card-core';
import { cardApi } from './card-api';
import { cardRefresh } from './card-state';
import { cardEntry, failureEntry, overlayResetEntry } from './log-core';
import { logInput } from './log-state';
import type { Host } from '@digi/workbench-core/host/host.ts';
import './card-panel.css';

const POLL_MS = 2000;
/** The device's name as the faceplate gives it. */
const deviceName = (id: string | undefined) =>
  id && id in panels ? panels[id as keyof typeof panels].name : (id ?? '').toUpperCase();

/** `reload`: start the loaded firmware again (the Restart button's path), which builds the card from the folder in use. */
export default function CardPanel(props: { host: Host; reload?: () => Promise<void> }): JSX.Element {
  const api = cardApi(props.host);
  const [card, setCard] = createSignal<CardFolder>();
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());

  let reading = false;
  /** One read at a time: a read that is due while one runs is not started (the light poll asks again). */
  const refresh = async () => {
    if (reading) return;
    reading = true;
    try {
      setCard(await api.folder());
      setError('');
    } catch (e) {
      setError(errorLine(e));
    } finally {
      reading = false;
    }
    setNow(Date.now());
  };
  // read when the strip is mounted, when a Transfer job ended, and on a light poll while it is mounted
  createEffect(() => {
    cardRefresh();
    untrack(() => void refresh());
  });
  const timer = window.setInterval(() => {
    if (!busy()) void refresh();
  }, POLL_MS);
  onCleanup(() => window.clearInterval(timer));

  const sync = async () => {
    setBusy(true);
    try {
      const next = await api.sync();
      setCard(next);
      setError('');
      logInput(cardEntry('synced', next));
    } catch (e) {
      setError(errorLine(e));
      logInput(failureEntry('card', 'sync', errorLine(e)));
    } finally {
      setBusy(false);
      setNow(Date.now());
    }
  };
  /** Run a call that answers with the folder state. */
  const change = async (what: string, run: () => Promise<CardFolder | undefined>) => {
    setBusy(true);
    try {
      const next = await run();
      if (next) {
        setCard(next);
        logInput(cardEntry(what, next));
      }
      setError('');
    } catch (e) {
      setError(errorLine(e));
      logInput(failureEntry('card', what, errorLine(e)));
    } finally {
      setBusy(false);
      setNow(Date.now());
    }
  };
  const choose = () =>
    change('folder chosen', () => {
      const c = card();
      return api.choose(c, `Choose the folder of the ${deviceName(c?.device)} card`);
    });
  const useDefault = () => change('default folder', () => api.useDefault());
  const reloadNow = () =>
    change('card reloaded', async () => {
      await props.reload?.();
      return api.folder();
    });
  /** Set the saved card changes aside (never deleted), then start the firmware again on the card without them. */
  const resetOverlay = async () => {
    setBusy(true);
    try {
      const r = await api.resetOverlay();
      logInput(overlayResetEntry(r));
      if (r.set_aside) await props.reload?.();
      setCard(await api.folder());
      setError('');
    } catch (e) {
      setError(errorLine(e));
      logInput(failureEntry('card', 'reset card changes', errorLine(e)));
    } finally {
      setBusy(false);
      setNow(Date.now());
    }
  };
  const openFolder = async () => {
    try {
      await api.openFolder();
    } catch (e) {
      setError(errorLine(e));
    }
  };

  return (
    <section class="card-mirror" aria-label="Emulator card folder">
      <div class="where">
        <Show when={card()?.device}>
          <strong class="device">{deviceName(card()!.device)}</strong>
        </Show>
        <span class="folder" title={card()?.folder ?? ''}>
          {card()?.folder ? shortFolder(card()!.folder) : 'No folder'}
        </span>
      </div>
      <p class="hint">The emulator copies its card store to this folder about 2 s after each write.</p>
      <div class="body">
        <div class="acts">
          <button type="button" class="act small" disabled={!card()?.device || busy()} onClick={() => void choose()}>
            Choose folder…
          </button>
          <Show when={card() && canUseDefault(card()!)}>
            <button type="button" class="act small" disabled={busy()} onClick={() => void useDefault()}>
              Use default
            </button>
          </Show>
          <button type="button" class="act small" disabled={!card()?.folder} onClick={() => void openFolder()}>
            Show in Finder
          </button>
          <button type="button" class="act small" disabled={!card()?.continuous || busy()} onClick={() => void sync()}>
            {busy() ? 'Working…' : 'Sync now'}
          </button>
          <button
            type="button"
            class="act small"
            disabled={!card()?.overlay || busy()}
            title={
              card()?.overlay
                ? 'Set the saved card changes aside and reload the firmware without them'
                : 'The card has no saved changes'
            }
            onClick={() => void resetOverlay()}
          >
            Reset card changes
          </button>
        </div>
        <Show when={card() && folderNotice(card()!)}>
          <div class="notice" role="status">
            <p>{folderNotice(card()!)}</p>
            <Show when={card()?.reload_needed && props.reload}>
              <button type="button" class="act small" disabled={busy()} onClick={() => void reloadNow()}>
                Reload now
              </button>
            </Show>
          </div>
        </Show>
        <Show when={card()}>
          <p class="status" classList={{ bad: !!card()?.error }}>
            {statusText(card()!, now())}
          </p>
        </Show>
        <Show when={error()}>
          <p class="err" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </section>
  );
}
