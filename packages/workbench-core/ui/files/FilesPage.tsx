// The Files page (dev-facing, read-only): a tree of every entry in the card store of the selected target, a usage bar like a partition manager,
// and an info pane with the .dkm header, the load state and the library match. The emulator and a device go through the same StoreOps
// (src/target-ops/ops.ts); nothing is written. The tree rows are the Transfer page's ListRow.
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from 'solid-js';
import { useTransferApp } from '../../src/transfer/app-context';
import { targetLabel } from '../../src/transfer/target';
import { failureLine } from '../../src/transfer/wait-core';
import { formatBytes } from '../../src/modules/backend';
import type { RowTag } from '../../src/transfer/transfer-core';
import type { StoreOps, StoreSnapshot } from '../../src/target-ops/ops';
import { sessionStoreOps } from '../../src/target-ops/session-store';
import {
  CATEGORIES,
  buildTree,
  categoryOf,
  hexDump,
  isDirEntry,
  libraryMarks,
  orphanPaths,
  visibleRows,
  type DkmHeader,
  type LibraryMark,
  type TreeNode,
} from '../../src/target-ops/store';
import { ListRow } from '../transfer/TransferParts';
import '../transfer/transfer-page.css';
import './files-page.css';

export interface FilesPageProps {
  /** The store to read; default: sessionStoreOps(() => app.target.session()), which follows the selected target. */
  store?: StoreOps;
  /** The built modules of the library for a device id (StoreFacts.device); default: app.loadLibrary. Errors only drop the library marks. */
  library?: (device: string) => Promise<readonly { path: string; hash: string }[]>;
  /** Read again when this value changes (desktop: cardRefresh after a Transfer job). */
  refresh?: () => unknown;
  /** Why nothing can be read now (desktop: the emulator is paused), or null. */
  halted?: () => string | null;
  /** Save a file the user picked; returns the saved path or null when cancelled. No Save button when absent. */
  save?: (name: string, bytes: Uint8Array) => Promise<string | null>;
  /** Shown under the header (desktop: the emulator card-folder strip). */
  extra?: JSX.Element;
}

const HEX_BYTES = 768;

const leafOf = (path: string): string => path.slice(path.lastIndexOf('/') + 1);
const isDkFile = (path: string): boolean => /\.(dkm|dks)$/.test(path);
const isLoadable = (path: string): boolean => isDkFile(path) && /^(modules|services)\//.test(path);
const plural = (n: number, one: string): string => `${n} ${one}${n === 1 ? '' : 's'}`;
const hex = (n: number): string => `0x${n.toString(16)}`;

function Field(props: { k: string; v: JSX.Element; plain?: boolean }): JSX.Element {
  return (
    <>
      <dt>{props.k}</dt>
      <dd classList={{ plain: props.plain }}>{props.v}</dd>
    </>
  );
}

export default function FilesPage(props: FilesPageProps): JSX.Element {
  const app = useTransferApp();
  const { target } = app.target;
  const store = props.store ?? sessionStoreOps(() => app.target.session());
  const library = props.library ?? ((device: string) => app.loadLibrary(device));

  const [snap, setSnap] = createSignal<StoreSnapshot>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [open, setOpen] = createSignal<ReadonlySet<string>>(new Set());
  const [selected, setSelected] = createSignal<string | null>(null);
  const [headers, setHeaders] = createSignal<ReadonlyMap<string, DkmHeader | null>>(new Map());
  const [marks, setMarks] = createSignal<ReadonlyMap<string, LibraryMark>>(new Map());
  const [orphans, setOrphans] = createSignal<ReadonlySet<string>>(new Set());
  // results for the selected file; cleared when the selection changes
  const [verifyText, setVerifyText] = createSignal('');
  const [hexText, setHexText] = createSignal<string | null>(null);
  const [saveText, setSaveText] = createSignal('');
  const [saving, setSaving] = createSignal(false);

  // headers by path + hash, so a changed file is read again; `gen` ends the background work of an older snapshot or target
  const cache = new Map<string, DkmHeader | null>();
  const inflight = new Map<string, Promise<void>>();
  let gen = 0;
  let seeded = false;
  let again = false;

  const tree = createMemo(() => buildTree(snap()?.entries ?? []));
  const index = createMemo(() => {
    const out = new Map<string, TreeNode>();
    const walk = (n: TreeNode) => {
      for (const c of n.children) {
        out.set(c.path, c);
        walk(c);
      }
    };
    walk(tree());
    return out;
  });
  const rows = createMemo(() => visibleRows(tree(), open()));
  const node = createMemo(() => {
    const p = selected();
    return p === null ? undefined : index().get(p);
  });

  const ensureHeader = (path: string, hash: string, my: number): Promise<void> => {
    const key = `${path}|${hash}`;
    const publish = (h: DkmHeader | null) => {
      if (my === gen) setHeaders((m) => new Map(m).set(path, h));
    };
    if (cache.has(key)) {
      publish(cache.get(key) ?? null);
      return Promise.resolve();
    }
    const have = inflight.get(key);
    if (have) return have;
    const p = store
      .header(path)
      .then((h) => {
        cache.set(key, h);
        publish(h);
      })
      .finally(() => inflight.delete(key));
    inflight.set(key, p);
    return p;
  };

  async function scan(s: StoreSnapshot, my: number): Promise<void> {
    try {
      // one at a time: SysEx replies come on one stream
      for (const e of s.entries) {
        if (isDirEntry(e) || !isLoadable(e.path)) continue;
        if (my !== gen) return;
        await ensureHeader(e.path.replace(/\/+$/, ''), e.hash, my);
      }
      if (my !== gen) return;
      const names: string[] = [];
      for (const e of s.entries) {
        const h = cache.get(`${e.path}|${e.hash}`);
        if (h && isLoadable(e.path)) names.push(h.name);
      }
      setOrphans(orphanPaths(s.entries, names));
    } catch (e) {
      console.info('Files: the header scan stopped:', e);
    }
  }

  async function marksFor(s: StoreSnapshot, my: number): Promise<void> {
    try {
      const lib = await library(s.facts.device);
      if (my === gen) setMarks(libraryMarks(s.entries, lib));
    } catch (e) {
      console.info('Files: no library marks:', e);
      if (my === gen) setMarks(new Map());
    }
  }

  async function load(): Promise<void> {
    if (props.halted?.()) return;
    if (busy()) {
      again = true;
      return;
    }
    const my = ++gen;
    const t = target();
    setBusy(true);
    setError('');
    try {
      const s = await store.snapshot();
      if (my !== gen) return;
      setSnap(s);
      if (!seeded) {
        seeded = true;
        setOpen(
          new Set(
            tree()
              .children.filter((c) => c.dir)
              .map((c) => c.path),
          ),
        );
      }
      const p = selected();
      if (p !== null && !index().has(p)) setSelected(null);
      setHeaders(
        new Map(
          s.entries.flatMap((e): [string, DkmHeader | null][] => {
            const k = `${e.path}|${e.hash}`;
            return cache.has(k) ? [[e.path, cache.get(k) ?? null]] : [];
          }),
        ),
      );
      setOrphans(new Set<string>());
      void marksFor(s, my);
      void scan(s, my);
    } catch (e) {
      if (my === gen) setError(failureLine(e, targetLabel(t)));
    } finally {
      if (my === gen) {
        setBusy(false);
        if (again) {
          again = false;
          void load();
        }
      }
    }
  }

  onMount(() => void load());
  onCleanup(() => {
    gen++;
  });
  // another target: what was read from the old one is not shown
  createEffect(
    on(
      target,
      () => {
        gen++;
        seeded = false;
        again = false;
        setBusy(false);
        setSnap(undefined);
        setSelected(null);
        setOpen(new Set<string>());
        setHeaders(new Map());
        setMarks(new Map());
        setOrphans(new Set<string>());
        setError('');
        void load();
      },
      { defer: true },
    ),
  );
  createEffect(
    on(
      () => props.refresh?.(),
      () => void load(),
      { defer: true },
    ),
  );
  createEffect(
    on(
      () => props.halted?.() ?? null,
      (h) => {
        if (h === null) void load();
      },
      { defer: true },
    ),
  );
  // another file: the results of the last one are not its own; a .dkm or .dks header is read when it is not known yet
  createEffect(
    on(selected, (p) => {
      setVerifyText('');
      setHexText(null);
      setSaveText('');
      const n = p === null ? undefined : index().get(p);
      if (n && !n.dir && n.hash !== null && isDkFile(n.path) && !headers().has(n.path)) {
        void ensureHeader(n.path, n.hash, gen).catch((e) => console.info('Files: header not read:', e));
      }
    }),
  );

  const toggle = (path: string) =>
    setOpen((o) => {
      const next = new Set(o);
      if (!next.delete(path)) next.add(path);
      return next;
    });

  const select = (path: string) => {
    setSelected(path);
    queueMicrotask(() =>
      document
        .querySelector(`.files-page .tree [data-key="${CSS.escape(path)}"]`)
        ?.scrollIntoView({ block: 'nearest' }),
    );
  };

  const onKey = (e: KeyboardEvent) => {
    const list = rows();
    const at = list.findIndex((r) => r.node.path === selected());
    const cur = at < 0 ? undefined : list[at]!.node;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const next = at < 0 ? 0 : Math.min(list.length - 1, Math.max(0, at + (e.key === 'ArrowDown' ? 1 : -1)));
      if (list[next]) select(list[next]!.node.path);
    } else if (e.key === 'ArrowRight' && cur?.dir && !open().has(cur.path)) {
      e.preventDefault();
      toggle(cur.path);
    } else if (e.key === 'ArrowLeft' && cur) {
      e.preventDefault();
      if (cur.dir && open().has(cur.path)) toggle(cur.path);
      else {
        const cut = cur.path.lastIndexOf('/');
        if (cut > 0) select(cur.path.slice(0, cut));
      }
    }
  };

  const stateOf = (path: string) =>
    isLoadable(path) ? snap()?.states.find((s) => s.name === leafOf(path)) : undefined;
  const moduleOf = (path: string) => {
    if (!isLoadable(path)) return undefined;
    const leaf = leafOf(path);
    const name = headers().get(path)?.name;
    const stem = leaf.replace(/\.dk[ms]$/, '');
    return snap()?.modules.find((m) => m.name === name || m.name === leaf || m.name === stem);
  };

  const tagsOf = (n: TreeNode): RowTag[] => {
    if (n.dir) return [];
    const out: RowTag[] = [];
    const st = stateOf(n.path);
    if (st) {
      out.push(
        st.state === 'ACTIVE'
          ? { text: 'loaded', tone: 'ok' }
          : { text: st.state.toLowerCase(), tone: st.state === 'FAILED' || st.state === 'REFUSED' ? 'warn' : 'dim' },
      );
    }
    const mark = marks().get(n.path);
    if (mark) out.push({ text: mark, tone: mark === 'other version' ? 'warn' : 'dim' });
    if (orphans().has(n.path))
      out.push({ text: 'orphan', tone: 'warn', title: 'No module on the card owns this file' });
    return out;
  };

  const usageBase = (): number => {
    const u = snap()?.usage;
    if (!u) return 0;
    return u.total ?? u.used.reduce((a, x) => a + x.bytes, 0) + u.free;
  };
  const pct = (bytes: number): string => `${usageBase() > 0 ? (100 * bytes) / usageBase() : 0}%`;
  const usageText = (): string => {
    const u = snap()?.usage;
    if (!u) return '';
    const used = u.used.map((x) => `${x.label} ${formatBytes(x.bytes)}`).join(', ');
    return `${used || 'Nothing used'}; free ${formatBytes(u.free)}${u.total === null ? '' : ` of ${formatBytes(u.total)}`}`;
  };

  const doVerify = async (n: TreeNode) => {
    setVerifyText('Verifying…');
    try {
      const h = await store.verify(n.path);
      if (selected() !== n.path) return;
      setVerifyText(
        h.toLowerCase() === (n.hash ?? '').toLowerCase()
          ? 'Device hash matches the table.'
          : `Device hash ${h} differs from the table hash.`,
      );
    } catch (e) {
      if (selected() === n.path) setVerifyText(failureLine(e, targetLabel(target())));
    }
  };

  const doHex = async (n: TreeNode) => {
    if (hexText() !== null) {
      setHexText(null);
      return;
    }
    setHexText('Reading…');
    try {
      const r = await store.read(n.path, 0, Math.min(HEX_BYTES, n.size));
      if (selected() === n.path) setHexText(r.bytes.length ? hexDump(r.bytes).join('\n') : '(empty)');
    } catch (e) {
      if (selected() === n.path) setHexText(failureLine(e, targetLabel(target())));
    }
  };

  const doSave = async (n: TreeNode) => {
    if (!props.save) return;
    setSaving(true);
    try {
      const bytes = await store.readAll(n.path, (done, size) => {
        if (selected() === n.path) setSaveText(`Reading ${done} of ${size}`);
      });
      const where = await props.save(leafOf(n.path), bytes);
      if (selected() === n.path) setSaveText(where ? `Saved to ${where}` : '');
    } catch (e) {
      if (selected() === n.path) setSaveText(failureLine(e, targetLabel(target())));
    } finally {
      setSaving(false);
    }
  };

  const Info = (p: { n: TreeNode }): JSX.Element => {
    const n = p.n;
    const cat = CATEGORIES.find((c) => c.key === categoryOf(n.path))?.label ?? '';
    const header = () => headers().get(n.path);
    const st = () => stateOf(n.path);
    const mod = () => moduleOf(n.path);
    const mark = () => marks().get(n.path);
    return (
      <>
        <Show
          when={!n.dir}
          fallback={
            <dl>
              <Field k="Path" v={n.path} />
              <Field k="Files" v={String(n.files)} />
              <Field k="Size" v={`${formatBytes(n.size)} (${n.size} bytes)`} />
            </dl>
          }
        >
          <dl>
            <Field k="Path" v={n.path} />
            <Field k="Size" v={`${formatBytes(n.size)} (${n.size} bytes)`} />
            <Field k="Hash" v={<span class="hash">{n.hash}</span>} />
            <Field k="Category" v={cat} plain />
            <Show when={mark()}>
              <Field
                k="Library"
                v={mark() === 'other version' ? 'A different build is in the library.' : 'Not in the library.'}
                plain
              />
            </Show>
            <Show when={orphans().has(n.path)}>
              <Field k="Orphan" v="No module on the card owns this file." plain />
            </Show>
          </dl>
          <Show when={isDkFile(n.path)}>
            <Show
              when={header()}
              fallback={<p class="note">{header() === null ? 'No valid DKMF header.' : 'Reading the header…'}</p>}
            >
              {(h) => (
                <>
                  <h4>Header</h4>
                  <dl>
                    <Field k="Name" v={h().name} />
                    <Field k="Role" v={typeof h().role === 'number' ? `unknown (${h().role})` : (h().role as string)} />
                    <Field k="Version" v={h().version} />
                    <Field k="Needs runtime" v={h().runtime} />
                    <Field k="Trace build" v={h().trace ? 'yes' : 'no'} />
                    <Show when={h().role === 'service'}>
                      <Field k="Service ABI" v={hex(h().svcAbi)} />
                    </Show>
                    <Field k="Image" v={`${h().imageSize} bytes at ${hex(h().imageOff)}`} />
                    <Field k="BSS" v={`${h().bssSize} bytes`} />
                    <Field k="DSP" v={`${h().dspSize} bytes`} />
                    <Field k="Relocations" v={String(h().relocCount)} />
                    <Field k="Imports" v={String(h().importCount)} />
                    <Field k="Table" v={`${h().tableSize} bytes`} />
                    <Field k="Format version" v={String(h().formatVersion)} />
                  </dl>
                </>
              )}
            </Show>
          </Show>
          <Show when={st() || mod()}>
            <h4>Load</h4>
            <dl>
              <Show when={st()}>
                {(s) => (
                  <>
                    <Field
                      k="State"
                      v={`${s().state} (result ${s().result}, detail ${hex(s().detail[0])} ${hex(s().detail[1])})`}
                    />
                  </>
                )}
              </Show>
              <Show when={mod()}>
                {(m) => (
                  <>
                    <Field k="CF base" v={hex(m().cf_base)} />
                    <Field k="CF size" v={`${m().cf_size} bytes`} />
                    <Field k="DSP length" v={`${m().dsp_len} bytes`} />
                    <Field k="Rows" v={String(m().rows)} />
                    <Field k="Faults" v={String(m().faults)} />
                    <Field k="Guard off" v={m().guard_off ? 'yes' : 'no'} />
                  </>
                )}
              </Show>
            </dl>
          </Show>
        </Show>
        <Show when={!n.dir}>
          <div class="acts">
            <button type="button" class="act small" onClick={() => void doVerify(n)}>
              Verify
            </button>
            <button type="button" class="act small" disabled={n.size === 0} onClick={() => void doHex(n)}>
              Hex
            </button>
            <Show when={props.save}>
              <button type="button" class="act small" disabled={saving()} onClick={() => void doSave(n)}>
                Save…
              </button>
            </Show>
            <button
              type="button"
              class="act small"
              onClick={() => void navigator.clipboard?.writeText(n.path).catch(() => {})}
            >
              Copy path
            </button>
          </div>
          <Show when={verifyText()}>
            <p class="note">{verifyText()}</p>
          </Show>
          <Show when={saveText()}>
            <p class="note">{saveText()}</p>
          </Show>
          <Show when={hexText() !== null}>
            <pre class="hex">{hexText()}</pre>
          </Show>
        </Show>
        <Show when={n.dir}>
          <div class="acts">
            <button
              type="button"
              class="act small"
              onClick={() => void navigator.clipboard?.writeText(n.path).catch(() => {})}
            >
              Copy path
            </button>
          </div>
        </Show>
      </>
    );
  };

  return (
    <section class="transfer-page files-page" aria-label="Files">
      <header class="head">
        <h2>Files</h2>
        <span class="meta">{targetLabel(target())}</span>
        <Show when={busy()}>
          <span class="meta">Reading…</span>
        </Show>
        <span class="sp" />
        <button type="button" class="act small" disabled={busy()} onClick={() => void load()}>
          Refresh
        </button>
      </header>
      {props.extra}
      <Show when={snap()?.facts}>
        {(f) => (
          <p class="facts">
            {f().device} · runtime {f().runtime} · protocol {f().protocol}
            <Show when={f().imageId}>{(id) => <> · image {id().slice(0, 8)}</>}</Show>
            <Show when={f().svcAbi !== null}>
              <> · service ABI {f().svcAbi}</>
            </Show>
            <Show when={f().cardReadOnly}>
              <span class="rtag warn">card read-only</span>
            </Show>
            <Show when={f().safeMode}>
              <span class="rtag warn">safe mode</span>
            </Show>
          </p>
        )}
      </Show>
      <Show when={props.halted?.()}>{(why) => <p class="note">{why()}</p>}</Show>
      <Show when={error()}>
        <p class="err" role="alert">
          <span class="msg">{error()}</span>
          <button type="button" class="act small" disabled={busy()} onClick={() => void load()}>
            Try again
          </button>
        </p>
      </Show>
      <Show when={snap()?.usage}>
        {(u) => (
          <div class="usagebox">
            <div class="usage" role="img" aria-label={usageText()}>
              <For each={u().used}>
                {(x) => (
                  <span
                    class={`seg seg-${x.key}`}
                    style={{ 'flex-basis': pct(x.bytes) }}
                    title={`${x.label} ${formatBytes(x.bytes)}, ${plural(x.files, 'file')}`}
                  />
                )}
              </For>
              <Show when={(u().overhead ?? 0) > 0}>
                <span
                  class="seg seg-overhead"
                  style={{ 'flex-basis': pct(u().overhead ?? 0) }}
                  title={`Overhead ${formatBytes(u().overhead ?? 0)}`}
                />
              </Show>
              <span
                class="seg seg-free"
                style={{ 'flex-basis': pct(u().free) }}
                title={`Free ${formatBytes(u().free)}`}
              />
            </div>
            <ul class="legend">
              <For each={u().used}>
                {(x) => (
                  <li>
                    <i class={`sw seg-${x.key}`} />
                    {x.label} {formatBytes(x.bytes)} · {plural(x.files, 'file')}
                  </li>
                )}
              </For>
              <Show when={(u().overhead ?? 0) > 0}>
                <li>
                  <i class="sw seg-overhead" />
                  Overhead {formatBytes(u().overhead ?? 0)}
                </li>
              </Show>
              <li>
                <i class="sw seg-free" />
                Free {formatBytes(u().free)}
              </li>
              <Show when={u().total !== null}>
                <li>Total {formatBytes(u().total ?? 0)}</li>
              </Show>
            </ul>
          </div>
        )}
      </Show>
      <div class="body">
        <div class="treebox">
          <Show when={snap()} fallback={<p class="empty">{busy() ? 'Reading…' : 'Nothing read yet.'}</p>}>
            <Show when={rows().length} fallback={<p class="empty">The store has no files.</p>}>
              <ul class="rows tree" tabindex="0" aria-label="Card store" onKeyDown={onKey}>
                <For each={rows()}>
                  {(r) => {
                    const n = r.node;
                    const version = () => (isDkFile(n.path) ? headers().get(n.path)?.version : undefined);
                    return (
                      <ListRow
                        rowKey={n.path}
                        indent={r.depth}
                        twisty={n.dir ? { open: open().has(n.path), onToggle: () => toggle(n.path) } : undefined}
                        folder={n.dir ? { disabled: false, onOpen: () => toggle(n.path) } : undefined}
                        selected={selected() === n.path}
                        onSelect={() => select(n.path)}
                        name={n.name}
                        size={formatBytes(n.size)}
                        title={n.dir ? `${n.path} (${plural(n.files, 'file')})` : n.path}
                        version={version()}
                        tags={tagsOf(n)}
                      />
                    );
                  }}
                </For>
              </ul>
            </Show>
          </Show>
        </div>
        <aside class="info" aria-label="Details">
          <Show when={node()} fallback={<p class="empty">Select a file or folder to see its details.</p>} keyed>
            {(n) => <Info n={n} />}
          </Show>
        </aside>
      </div>
    </section>
  );
}
