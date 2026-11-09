// The Modules page: filter the shipped modules, pick some, and hand the picks to the Transfer page.
import { For, Show, createMemo, createSignal } from 'solid-js';
import type { Category } from '../data/module-labels';
import type { ModuleInfo } from '../data/modules';
import ModuleCard from './ModuleCard';
import './module-card.css';

export interface ModuleBrowserProps {
  modules: ModuleInfo[];
  /** The site base path, ending in a slash. */
  base: string;
}

/** Filter button labels (plural), in button order; only the categories present are shown. */
const FILTER_LABEL: Partial<Record<Category, string>> = {
  machines: 'Machines',
  fx: 'Effects',
  tools: 'Tools',
};

export default function ModuleBrowser(props: ModuleBrowserProps) {
  const [category, setCategory] = createSignal<'all' | Category>('all');
  const [device, setDevice] = createSignal<'any' | 'dt2' | 'dn2'>('any');
  const [added, setAdded] = createSignal<Set<string>>(new Set());

  const categories = createMemo(() => {
    const present = new Set(props.modules.map((m) => m.category));
    return (Object.keys(FILTER_LABEL) as Category[]).filter((c) => present.has(c));
  });
  const shown = createMemo(() =>
    props.modules.filter(
      (m) =>
        (category() === 'all' || m.category === category()) &&
        (device() === 'any' || m.devices.includes(device() as 'dt2' | 'dn2')),
    ),
  );
  const toggle = (id: string) =>
    setAdded((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  return (
    <>
      <div class="browse-bar">
        <button type="button" aria-pressed={category() === 'all'} onClick={() => setCategory('all')}>
          All
        </button>
        <For each={categories()}>
          {(c) => (
            <button type="button" aria-pressed={category() === c} onClick={() => setCategory(c)}>
              {FILTER_LABEL[c]}
            </button>
          )}
        </For>
        <span class="browse-spacer" />
        <label>
          Device
          <select value={device()} onChange={(e) => setDevice(e.currentTarget.value as 'any' | 'dt2' | 'dn2')}>
            <option value="any">Any device</option>
            <option value="dt2">Digitakt II</option>
            <option value="dn2">Digitone II</option>
          </select>
        </label>
      </div>
      <Show when={shown().length > 0} fallback={<p class="browse-empty">No modules match.</p>}>
        <ul class="cards">
          <For each={shown()}>
            {(m) => (
              <li>
                <ModuleCard module={m} added={added().has(m.id)} onToggle={() => toggle(m.id)} />
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={added().size > 0}>
        <div class="browse-selected">
          <span>{added().size} selected</span>
          <a class="btn primary" href={`${props.base}transfer/?add=${encodeURIComponent([...added()].join(','))}`}>
            Go to Transfer
          </a>
        </div>
      </Show>
    </>
  );
}
