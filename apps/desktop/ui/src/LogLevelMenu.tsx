// The Level button of the Log drawer and its menu: the default log level and one row for each loaded module, set on the origin chosen in Source.
// The rules (reading LOGCFG, the 10-byte set request, the texts) are in log-level-core.ts. The levels live in the runtime's RAM and start again at
// its next boot. The debug module's state has its own indicator in LogPanel.tsx; the levels exist only while the module is loaded.
import { For, Show, createEffect, createSignal, onCleanup, on } from 'solid-js';
import type { Origin } from './log-core';
import type { LogFollow } from './log-follow';
import { levelChoices, levelLabel, type LevelChange, type LevelView } from './log-level-core';

export interface LogLevelMenuProps {
  follow: LogFollow;
  /** The origin the menu acts on; undefined when Source does not say. */
  origin(): Origin | undefined;
  /** Why the menu is off (log-level-core.ts `levelsOff`), or ''. */
  off(): string;
}

export default function LogLevelMenu(props: LogLevelMenuProps) {
  const [open, setOpen] = createSignal(false);
  const [view, setView] = createSignal<LevelView>();
  const [busy, setBusy] = createSignal(false);
  const [problem, setProblem] = createSignal('');
  let root: HTMLSpanElement | undefined;

  const cfg = () => {
    const o = props.origin();
    return o ? props.follow.config()[o] : undefined;
  };
  const label = () =>
    props.off()
      ? 'Level'
      : levelLabel(view()?.level ?? cfg()?.level, view()?.modules.map((m) => m.level) ?? cfg()?.modules);

  const load = async () => {
    const origin = props.origin();
    if (!origin) return;
    setBusy(true);
    setProblem('');
    try {
      const v = await props.follow.levels(origin);
      if (v) setView(v);
      else setOpen(false); // no LOGCFG: `follow.logcfg()` says so and the button is off
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const change = async (c: LevelChange) => {
    const origin = props.origin();
    if (!origin || busy()) return;
    setBusy(true);
    setProblem('');
    try {
      setView(await props.follow.setLevel(origin, c));
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // another origin, or the menu is off: the old state is not shown
  createEffect(
    on([() => props.origin(), () => !!props.off()], () => {
      setView(undefined);
      setProblem('');
      if (props.off()) setOpen(false);
    }),
  );
  createEffect(() => {
    if (!open()) return;
    void load();
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

  const Row = (p: {
    name: string;
    title?: string;
    level: number;
    kind: 'default' | 'module';
    change(level: number): LevelChange;
  }) => (
    <div class="level-row" role="group" aria-label={`${p.name} log level`}>
      <span class="level-name" title={p.title ?? p.name}>
        {p.name}
      </span>
      <span class="level-seg">
        <For each={levelChoices(p.kind)}>
          {(c) => (
            <button
              type="button"
              classList={{ on: c.value === p.level }}
              aria-pressed={c.value === p.level}
              disabled={busy()}
              onClick={() => void change(p.change(c.value))}
            >
              {c.label}
            </button>
          )}
        </For>
      </span>
    </div>
  );

  return (
    <span class="level-wrap" ref={root}>
      <button
        type="button"
        class="tb small"
        classList={{ on: open() }}
        aria-haspopup="true"
        aria-expanded={open()}
        disabled={!!props.off()}
        title={props.off() || `Set the log levels of the ${props.origin()}; they start again at its next boot`}
        onClick={() => setOpen(!open())}
      >
        {label()} <span aria-hidden="true">▾</span>
      </button>
      <Show when={open()}>
        <div class="level-pop" role="dialog" aria-label={`Log levels of the ${props.origin()}`}>
          <Show when={view()} fallback={<p class="level-note">{problem() || 'Reading the levels…'}</p>}>
            {(v) => (
              <>
                <Row name="Default" level={v().level} kind="default" change={(level) => ({ what: 'default', level })} />
                <For each={v().modules}>
                  {(m) => (
                    <Row
                      name={m.name}
                      level={m.level}
                      kind="module"
                      change={(level) => ({ what: 'module', index: m.index, level })}
                    />
                  )}
                </For>
                <Show when={v().modules.length === 0}>
                  <p class="level-note">No module is loaded.</p>
                </Show>
                <p class="level-note">
                  {problem() ||
                    `A module at "default" follows the Default level. Levels are kept in RAM and reset at the next boot of the ${props.origin()}.`}
                </p>
              </>
            )}
          </Show>
        </div>
      </Show>
    </span>
  );
}
