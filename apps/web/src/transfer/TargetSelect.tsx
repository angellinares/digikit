// The target selector: the emulator (where the app has one), or a MIDI port with a device on it (env.ts). Used in the Transfer and Firmware pages
// of the site, and once in the desktop app's title bar (TitleBar.tsx), which every page shares. The list of ports is read when the page opens and with the rescan button (a page with its own Refresh passes `reload`: the
// list is read again when that number changes, and the button is left out); the port in use stays in the list.
import { For, Show, createEffect, createSignal, on, onMount, type JSX } from 'solid-js';
import { errorLine } from '../modules/backend';
import { noteConnection } from './connection-state';
import { transferEnv } from './env';
import { selectTarget, target, targetLabel, type Target } from './target';
import './target-select.css';

const VALUE_EMU = 'emulator';
const VALUE_NONE = '';

/** Read the ports again in every selector shown (the desktop's title bar has the one selector; its pages' Refresh asks through this). */
const [portScan, setPortScan] = createSignal(0);
export const rescanTargets = (): void => void setPortScan((n) => n + 1);

export default function TargetSelect(props: {
  disabled?: boolean;
  onError?: (m: string) => void;
  reload?: () => number;
}): JSX.Element {
  const [ports, setPorts] = createSignal<string[]>([]);
  const rescan = async () => {
    try {
      const names = await transferEnv().targets.ports();
      setPorts(names);
      // the port of the target is not listed any more: the device was unplugged
      const t = target();
      if (t.kind === 'midi' && !names.includes(t.port)) noteConnection({ type: 'closed' });
    } catch (e) {
      props.onError?.(errorLine(e));
    }
  };
  onMount(() => void rescan());
  createEffect(
    on(
      () => [props.reload?.(), portScan()],
      () => void rescan(),
      { defer: true },
    ),
  );
  const options = () => {
    const t = target();
    const names = ports();
    return t.kind === 'midi' && !names.includes(t.port) ? [t.port, ...names] : names;
  };
  const selectValue = () => {
    const t = target();
    return t.kind === 'emulator' ? VALUE_EMU : t.kind === 'none' ? VALUE_NONE : t.port;
  };
  const pick = async (value: string) => {
    const next: Target =
      value === VALUE_EMU
        ? { kind: 'emulator' }
        : value === VALUE_NONE
          ? { kind: 'none' }
          : { kind: 'midi', port: value };
    try {
      await selectTarget(next);
    } catch (e) {
      props.onError?.(errorLine(e));
    }
  };
  return (
    <span class="target-select">
      <label>
        <span class="dim">Target</span>{' '}
        <select
          aria-label="Target"
          disabled={props.disabled}
          value={selectValue()}
          onChange={(e) => void pick(e.currentTarget.value)}
        >
          <Show when={target().kind === 'none'}>
            <option value={VALUE_NONE}>{options().length ? 'Pick a MIDI port' : 'No MIDI port found'}</option>
          </Show>
          <Show when={transferEnv().targets.emulator}>
            <option value={VALUE_EMU}>Emulator</option>
          </Show>
          <For each={options()}>
            {(name) => <option value={name}>{targetLabel({ kind: 'midi', port: name })}</option>}
          </For>
        </select>
      </label>
      <Show when={!props.reload}>
        <button
          type="button"
          class="act small"
          disabled={props.disabled}
          title="Look for MIDI ports again"
          onClick={() => void rescan()}
        >
          Rescan
        </button>
      </Show>
      <Show when={transferEnv().targets.emulator && target().kind === 'midi'}>
        <span class="dim">a real device</span>
      </Show>
    </span>
  );
}
