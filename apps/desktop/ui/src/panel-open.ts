// The right-hand column of the Emulator and Remote pages (SidePanels.tsx): which tab it shows (Card, Memory, Metrics) and whether it is open or
// folded to a rail. Both choices are kept in localStorage; a page that lacks the chosen tab (Remote has Memory only) shows its first one.
import { createSignal, type Accessor } from 'solid-js';
import { TABS, type InspectorTab } from './inspector-core';

/** Below this window width the column starts folded, unless the user chose (it would take a quarter of the window). */
export const WIDE_PX = 1100;

function persisted<T>(
  key: string,
  parse: (saved: string) => T | undefined,
  initial: () => T,
): [Accessor<T>, (next: T) => void] {
  const read = () => {
    try {
      const saved = localStorage.getItem(key);
      const value = saved === null ? undefined : parse(saved);
      if (value !== undefined) return value;
    } catch {
      /* no choice kept */
    }
    return initial();
  };
  const [value, set] = createSignal<T>(read());
  return [
    value,
    (next) => {
      set(() => next);
      try {
        localStorage.setItem(key, String(next));
      } catch {
        /* the choice lasts until the window closes */
      }
    },
  ];
}

export const [inspectorTab, setInspectorTab] = persisted<InspectorTab>(
  'digikit.inspectorTab',
  (s) => ((TABS as string[]).includes(s) ? (s as InspectorTab) : undefined),
  () => 'card',
);
export const [inspectorOpen, setInspectorOpen] = persisted<boolean>(
  'digikit.inspector',
  (s) => (s === 'true' ? true : s === 'false' ? false : undefined),
  () => window.innerWidth >= WIDE_PX,
);
