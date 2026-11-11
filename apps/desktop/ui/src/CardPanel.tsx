// The Card panel of the Emulator page: the host folder of the loaded device's card (`plus_drive/<device>/` or the folder the user chose for
// the device). The module store is mirrored to it (kept up to date by the emulator itself about 2 s after the guest's last card write;
// one way, card to folder) and the card of a load is built from it. It shows the device and the folder, the module files with their
// sizes, the time of the last sync, "Choose folder...", "Use default", "Show in Finder" and "Sync now". A device whose card is built
// from the folder (the Digitakt II) takes a new folder at the next load: the panel says so and offers "Reload now". The emulator does the
// work and keeps the choice per device (`card.folder`, `card.folder_set`, `card.sync` through `emu_call`); this panel only asks.
import { invoke } from '@tauri-apps/api/core';
import { For, Show, createEffect, createMemo, createSignal, onCleanup, untrack, type JSX } from 'solid-js';
import { errorLine, formatBytes } from '@web/modules/backend';
import { panels } from '@web/data/faceplates';
import {
  canUseDefault,
  fileGroups,
  fileRows,
  folderNotice,
  nameInGroup,
  pickStart,
  shortFolder,
  statusText,
  type CardFolder,
  type OverlayReset,
} from './card-core';
import { cardRefresh } from './card-state';
import { cardEntry, failureEntry, overlayResetEntry } from './log-core';
import { logInput } from './log-state';
import './card-panel.css';

const POLL_MS = 2000;
const call = <T,>(method: string, params: Record<string, unknown> = {}) =>
  invoke<T>('emu_call', { method, params, session: null });
/** The device's name as the faceplate gives it. */
const deviceName = (id: string | undefined) =>
  id && id in panels ? panels[id as keyof typeof panels].name : (id ?? '').toUpperCase();

/** `reload`: start the loaded firmware again (the Restart button's path), which builds the card from the folder in use. */
export default function CardPanel(props: { visible: boolean; reload?: () => Promise<void> }): JSX.Element {
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
      setCard(await call<CardFolder>('card.folder'));
      setError('');
    } catch (e) {
      setError(errorLine(e));
    } finally {
      reading = false;
    }
    setNow(Date.now());
  };
  // Only a change of whether the panel is on screen and open counts, not each change of what `visible` reads (the Console's `status` changes
  // once per display frame): a memo passes on a change of its value only.
  const active = createMemo(() => props.visible);
  // read when the panel is shown and opened, when a Transfer job ended, and on a light poll while it is visible
  createEffect(() => {
    cardRefresh();
    if (active()) untrack(() => void refresh());
  });
  const timer = window.setInterval(() => {
    if (active() && !busy()) void refresh();
  }, POLL_MS);
  onCleanup(() => window.clearInterval(timer));

  const sync = async () => {
    setBusy(true);
    try {
      const next = await call<CardFolder>('card.sync');
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
    change('folder chosen', async () => {
      const c = card();
      const dir = await invoke<string | null>('local_pick_folder', {
        start: c ? pickStart(c) : null,
        title: `Choose the folder of the ${deviceName(c?.device)} card`,
      });
      return dir ? call<CardFolder>('card.folder_set', { dir }) : undefined;
    });
  const useDefault = () => change('default folder', () => call<CardFolder>('card.folder_set', {}));
  const reloadNow = () =>
    change('card reloaded', async () => {
      await props.reload?.();
      return call<CardFolder>('card.folder');
    });
  /** Set the saved card changes aside (never deleted), then start the firmware again on the card without them. */
  const resetOverlay = async () => {
    setBusy(true);
    try {
      const r = await call<OverlayReset>('card.reset_overlay');
      logInput(overlayResetEntry(r));
      if (r.set_aside) await props.reload?.();
      setCard(await call<CardFolder>('card.folder'));
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
      await invoke('card_open_folder');
    } catch (e) {
      setError(errorLine(e));
    }
  };

  return (
    <aside class="card-panel" aria-label="Card" hidden={!props.visible}>
      <div class="card-body">
        <Show when={card()?.device}>
          <h3 class="device">{deviceName(card()!.device)}</h3>
        </Show>
        <p class="folder" title={card()?.folder ?? ''}>
          {card()?.folder ? shortFolder(card()!.folder) : 'No folder'}
        </p>
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
        <Show
          when={card() && fileRows(card()!).length}
          fallback={<p class="empty">{card()?.continuous ? 'No files in the folder.' : ''}</p>}
        >
          <For each={fileGroups(card()!)}>
            {(g) => (
              <div class="group">
                <Show when={g.folder}>
                  <h4 title={`${g.files.length} file${g.files.length === 1 ? '' : 's'}`}>
                    <span>{g.folder}/</span>
                    <span class="size">{formatBytes(g.bytes)}</span>
                  </h4>
                </Show>
                <ul class="files">
                  <For each={g.files}>
                    {(f) => (
                      <li title={`${f.path}\nhash ${f.hash}`}>
                        <span class="name">{nameInGroup(f.path, g.folder)}</span>
                        <span class="size">{formatBytes(f.bytes)}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            )}
          </For>
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
    </aside>
  );
}
