// One module on the home page: icon, name, one-line summary, the devices it is for, and a link to the Transfer page, where it is installed.
import { For, Show } from 'solid-js';
import { CATEGORY_LABEL, DEVICE_SHORT } from '../data/module-labels';
import type { ModuleInfo } from '../data/modules';
import './module-card.css';

export interface ModuleCardProps {
  module: ModuleInfo;
  /** Where the Install link goes (the Transfer page). Not used when `onToggle` is given. */
  href?: string;
  /** The module is in the selection (the module browser). */
  added?: boolean;
  /** Shows an Add button in place of the Install link. */
  onToggle?: () => void;
}

export default function ModuleCard(props: ModuleCardProps) {
  return (
    <article class="card">
      <div class="card-top">
        {/* oxlint-disable-next-line solid/no-innerhtml -- repository SVGs, checked by apps/web/tests/icons.test.mjs */}
        <span class="tile" innerHTML={props.module.icon} />
        <span class="eyebrow">{CATEGORY_LABEL[props.module.category]}</span>
      </div>
      <div class="card-text">
        <h3>{props.module.name}</h3>
        <p class="sum">{props.module.summary}</p>
        <Show when={props.module.machines.some((m) => m.params.length > 0)}>
          <p class="knobs">Knobs: {props.module.machines.flatMap((m) => m.params.map((p) => p.short)).join(', ')}</p>
        </Show>
      </div>
      <div class="grow" />
      <div class="facts-row">
        <div class="devs">
          <For each={props.module.devices}>{(d) => <span class="dev">{DEVICE_SHORT[d]}</span>}</For>
        </div>
        <Show when={props.module.author}>
          <span class="by">by {props.module.author}</span>
        </Show>
      </div>
      <div class="card-foot">
        <Show when={!props.onToggle}>
          <span class="id" title={`${props.module.id} v${props.module.version}`}>
            {props.module.id}
          </span>
        </Show>
        <Show
          when={props.onToggle}
          fallback={
            <a class="add" href={props.href}>
              Install
            </a>
          }
        >
          <a class="src" href={`https://github.com/ElekLabs/digikit/tree/main/${props.module.dir}`}>
            Source
          </a>
          <button type="button" class="add" aria-pressed={!!props.added} onClick={props.onToggle}>
            {props.added ? 'Added' : 'Add'}
          </button>
        </Show>
      </div>
    </article>
  );
}
