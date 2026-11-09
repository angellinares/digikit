// The Default modules drop-down: the run-time modules that are installed and enabled together with the firmware (the desktop app: `[digikit]
// default_modules` of the device file). The same control stands in the Firmware pane and next to the emulator's firmware file. The user can
// untick an entry; the choice is kept per device (default-modules-state.ts). Renders nothing where the app gives no default modules (the site).
import { For, Show, createSignal, onCleanup, createEffect, type JSX } from 'solid-js';
import { defaultsLabel } from './default-modules-core';
import { entriesFor, setTick, tickedFor, unticked } from './default-modules-state';
import { firmwareEnv } from './env';
import './default-modules.css';

export interface DefaultModulesProps {
  /** The device id (`dt2`, `dn2`) the list is for; undefined until it is known. */
  device: string | undefined;
  /** Why the control is off (shown as its title), or empty. */
  off?: string;
  /** What the note under the list says. */
  note: string;
}

export default function DefaultModules(props: DefaultModulesProps): JSX.Element {
  const [open, setOpen] = createSignal(false);
  let root: HTMLSpanElement | undefined;
  const entries = () => (props.device ? entriesFor(props.device) : []);
  const ticked = () => (props.device ? tickedFor(props.device).length : 0);
  const off = () => props.off || (props.device ? '' : 'Choose the firmware file first: the list is the device’s');
  createEffect(() => {
    if (!open()) return;
    const away = (e: Event) => {
      if (root && !root.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', key);
    onCleanup(() => {
      document.removeEventListener('pointerdown', away);
      document.removeEventListener('keydown', key);
    });
  });
  return (
    <Show when={firmwareEnv().defaultModules}>
      <span class="defmods" ref={root}>
        <button
          type="button"
          class="defmods-btn"
          classList={{ on: open() }}
          aria-haspopup="true"
          aria-expanded={open()}
          disabled={!!off()}
          title={off() || 'The modules installed and enabled together with the firmware'}
          onClick={() => setOpen(!open())}
        >
          {defaultsLabel(ticked())} <span aria-hidden="true">▾</span>
        </button>
        <Show when={open() && props.device}>
          {(device) => (
            <div class="defmods-pop" role="dialog" aria-label="Default modules">
              <For each={entries()} fallback={<p class="defmods-note">This device has no default modules.</p>}>
                {(e) => (
                  <label class="defmods-row">
                    <input
                      type="checkbox"
                      checked={!(unticked()[device()] ?? []).includes(e.id)}
                      onChange={(ev) => setTick(device(), e.id, ev.currentTarget.checked)}
                    />
                    <span class="defmods-id">{e.id}</span>
                    <span class="defmods-sum">{e.summary}</span>
                  </label>
                )}
              </For>
              <Show when={entries().length > 0}>
                <p class="defmods-note">{props.note}</p>
              </Show>
            </div>
          )}
        </Show>
      </span>
    </Show>
  );
}
