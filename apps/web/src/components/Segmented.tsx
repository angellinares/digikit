// A segmented switch: one choice of a few, as buttons in a radiogroup. Arrow keys, Home and End move the choice; only the chosen button is in
// the tab order. A thumb slides behind the chosen button (the buttons share the width equally).
import { For, type JSX } from 'solid-js';
import './segmented.css';

export type SegmentedOption<T extends string> = { id: T; label: string; title?: string };

export default function Segmented<T extends string>(props: {
  label: string;
  options: readonly SegmentedOption<T>[];
  value: T;
  onChange: (value: T) => void;
  disabled?: boolean;
}): JSX.Element {
  let el: HTMLSpanElement | undefined;
  const index = () =>
    Math.max(
      0,
      props.options.findIndex((o) => o.id === props.value),
    );
  const nextIndex = (key: string, i: number, n: number): number => {
    if (key === 'ArrowRight' || key === 'ArrowDown') return (i + 1) % n;
    if (key === 'ArrowLeft' || key === 'ArrowUp') return (i - 1 + n) % n;
    if (key === 'Home') return 0;
    if (key === 'End') return n - 1;
    return -1;
  };
  const onKey = (e: KeyboardEvent) => {
    const next = nextIndex(e.key, index(), props.options.length);
    if (next < 0) return;
    e.preventDefault();
    if (props.disabled) return;
    const o = props.options[next]!;
    if (o.id !== props.value) props.onChange(o.id);
    el?.querySelectorAll<HTMLButtonElement>('button')[next]?.focus();
  };
  return (
    <span
      ref={el}
      class="segmented"
      role="radiogroup"
      aria-label={props.label}
      onKeyDown={onKey}
      style={{ '--seg-i': String(index()), '--seg-n': String(props.options.length) }}
    >
      <span class="seg-thumb" aria-hidden="true" />
      <For each={props.options}>
        {(o) => (
          <button
            type="button"
            role="radio"
            aria-checked={o.id === props.value}
            tabindex={o.id === props.value ? 0 : -1}
            classList={{ on: o.id === props.value }}
            disabled={props.disabled}
            title={o.title}
            onClick={() => o.id !== props.value && props.onChange(o.id)}
          >
            {o.label}
          </button>
        )}
      </For>
    </span>
  );
}
