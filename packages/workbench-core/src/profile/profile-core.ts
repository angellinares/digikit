// The pure logic of the Profile pane (apps/desktop/ui/src/ProfilePanel.tsx): rows per owner from the runtime's span counters, the share of the
// ColdFire each owner takes, the 60-second graph and its markers, the "max far above mean" rule, and the comparison with an exported profile. No DOM,
// no Tauri, no module state: `createHistory` is a closure the panel creates. Host tests: tests/profile-core.test.ts.
//
// The data is `profileAll` of dk-session (PROFILE_STATE and PROFILE_READ of the debug module): per owner `calls`, `sumTicks` and `maxTicks` of the
// span clock, and the elapsed window in ticks, so a share is `sumTicks / elapsedTicks` on the emulator (emulated time) and on a device alike.
import { SPAN_PHASES } from '@digi/dk-session/gen_proto';

/** The type of an owner, which sets its colour (the Memory panel's): a module's library group, or the runtime's own phases and polls. */
export type OwnerKind = 'machine' | 'effect' | 'tool' | 'runtime' | 'other';

/** The stack order of the graph, bottom first. */
export const STACK: readonly OwnerKind[] = ['runtime', 'other', 'tool', 'effect', 'machine'];

/** A span as `profileAll` gives it. */
export interface SpanIn {
  row: number;
  kind: 'module' | 'phase' | 'poll';
  /** The module record index, the phase number or the poll slot. */
  index: number;
  calls: number;
  sumTicks: bigint;
  maxTicks: number;
}

/**
 * A used poll slot: the module record that registered the function (`'runtime'`: the firmware image, a patched-in core module), the function's
 * address, and its name when the address map names it (`name`, set by the backend). An empty slot is `undefined`.
 */
export interface PollInfo {
  owner: number | 'runtime';
  fn: number;
  name?: string;
}

/** One reading of the device. */
export interface Snapshot {
  running: boolean;
  hz: number;
  /** Ticks of the span clock that passed while running since the last reset. */
  elapsedTicks: bigint;
  spans: SpanIn[];
  /** The poll slots (index = slot); empty when the debug module is too old to say. */
  polls?: (PollInfo | undefined)[];
}

/** A loaded module: the record index the spans use, its name and its type (from the library; `other` when the library does not know it). */
export interface ModuleRef {
  index: number;
  name: string;
  kind: OwnerKind;
}

export type Group = 'modules' | 'phases' | 'polls';
export const GROUP_LABEL: Record<Group, string> = { modules: 'Modules', phases: 'Runtime phases', polls: 'Polls' };
export const GROUPS: readonly Group[] = ['modules', 'phases', 'polls'];

export interface Row {
  /** `module:3`, `phase:1`, `poll:0`: stable across readings. */
  id: string;
  group: Group;
  name: string;
  kind: OwnerKind;
  calls: number;
  sumTicks: bigint;
  maxTicks: number;
  /** Of the elapsed window, 0 to 1 (more than 1 only when the clock reads wrong). */
  share: number;
  meanUs: number;
  maxUs: number;
  /** The longest call is far above the mean (`isHot`). */
  hot: boolean;
}

/**
 * A long call that the mean hides: the maximum is more than 4 times the mean and at least 100 µs. Below 100 µs a spike does not hold up the UI loop
 * (one pass is about 20 ms), so a poll with a mean of 5 µs and a worst call of 60 µs is not marked.
 */
export function isHot(meanUs: number, maxUs: number): boolean {
  return maxUs >= 100 && maxUs > 4 * meanUs;
}

const toUs = (ticks: number, hz: number) => (hz > 0 ? (ticks * 1e6) / hz : 0);

/** The share of the window: `sum / elapsed`, in 1/10000 steps (the sums are 64 bits; the quotient is a small number). */
export function shareOf(sum: bigint, elapsed: bigint): number {
  if (elapsed <= 0n) return 0;
  return Number((sum * 10000n) / elapsed) / 10000;
}

/**
 * The owner a poll slot shows as: `<module> poll` for the module that registered it, `runtime poll` for the firmware image, each with `: <function>`
 * when the address map names the function; `poll i` when the slot is unknown. The poll set changes at run time (`dkrt_poll_add`), so the slots are
 * read with the spans; the phases are in the contract (`SPAN_PHASES`, sdk/sdk.toml `[spans]`).
 */
export function ownerName(
  s: Pick<SpanIn, 'kind' | 'index'>,
  modules: ModuleRef[],
  polls?: (PollInfo | undefined)[],
): string {
  if (s.kind === 'module') return modules.find((m) => m.index === s.index)?.name ?? `module ${s.index}`;
  if (s.kind === 'phase') return SPAN_PHASES[s.index] ?? `phase ${s.index}`;
  const p = polls?.[s.index];
  if (!p) return `poll ${s.index}`;
  const owner =
    p.owner === 'runtime' ? 'runtime' : (modules.find((m) => m.index === p.owner)?.name ?? `module ${p.owner}`);
  return p.name ? `${owner} poll: ${p.name}` : `${owner} poll`;
}

const kindOf = (s: SpanIn, modules: ModuleRef[]): OwnerKind =>
  s.kind === 'module' ? (modules.find((m) => m.index === s.index)?.kind ?? 'other') : 'runtime';

/** The rows of a reading: every owner that ran, heaviest first within its group. */
export function buildRows(snap: Snapshot, modules: ModuleRef[]): Row[] {
  const rows: Row[] = [];
  for (const s of snap.spans) {
    if (s.calls === 0) continue;
    const meanUs = toUs(Number(s.sumTicks) / s.calls, snap.hz);
    const maxUs = toUs(s.maxTicks, snap.hz);
    rows.push({
      id: `${s.kind}:${s.index}`,
      group: s.kind === 'module' ? 'modules' : s.kind === 'phase' ? 'phases' : 'polls',
      name: ownerName(s, modules, snap.polls),
      kind: kindOf(s, modules),
      calls: s.calls,
      sumTicks: s.sumTicks,
      maxTicks: s.maxTicks,
      share: shareOf(s.sumTicks, snap.elapsedTicks),
      meanUs,
      maxUs,
      hot: isHot(meanUs, maxUs),
    });
  }
  return rows.sort((a, b) => (a.sumTicks === b.sumTicks ? 0 : a.sumTicks > b.sumTicks ? -1 : 1));
}

/** The rows by group, in the panel's order; a group without rows is left out. */
export function groupRows(rows: Row[]): { group: Group; label: string; rows: Row[] }[] {
  return GROUPS.map((group) => ({
    group,
    label: GROUP_LABEL[group],
    rows: rows.filter((r) => r.group === group),
  })).filter((g) => g.rows.length > 0);
}

/** The ticks of each kind in a reading. */
export function kindSums(snap: Snapshot, modules: ModuleRef[]): Record<OwnerKind, bigint> {
  const sums: Record<OwnerKind, bigint> = { machine: 0n, effect: 0n, tool: 0n, runtime: 0n, other: 0n };
  for (const s of snap.spans) sums[kindOf(s, modules)] += s.sumTicks;
  return sums;
}

/** All owners together, of the whole window: the mean ColdFire load of the recording. */
export function totalShare(snap: Snapshot): number {
  return shareOf(
    snap.spans.reduce((a, s) => a + s.sumTicks, 0n),
    snap.elapsedTicks,
  );
}

// ---- the graph ------------------------------------------------------------------------------------------------------------------------

/** One column of the graph: the share each kind took of one reading interval, 0 to 1. */
export type Column = Record<OwnerKind, number>;
export interface Marker {
  /** The column the marker stands at (0 is the oldest column that is shown). */
  col: number;
  text: string;
}

export interface History {
  /** Add a reading; the interval since the last one becomes a column. A reading that went back (a reset) starts the graph again. */
  push(snap: Snapshot, modules: ModuleRef[]): void;
  /** The newest `max` columns, oldest first. */
  columns(): Column[];
  markers(): Marker[];
  /** The share of the newest interval, all kinds together. */
  now(): number;
  clear(): void;
}

const shortName = (n: string) => n.slice(n.lastIndexOf('/') + 1);

/** The history of one recording: at most `max` columns (one per reading, about one a second: 60 s). */
export function createHistory(max = 60): History {
  let cols: { seq: number; col: Column }[] = [];
  let marks: { seq: number; text: string }[] = [];
  let seq = 0;
  let prev: { elapsed: bigint; sums: Record<OwnerKind, bigint>; names: string[] } | undefined;
  const clear = () => {
    cols = [];
    marks = [];
    prev = undefined;
  };
  return {
    clear,
    push(snap, modules) {
      const sums = kindSums(snap, modules);
      const names = modules.map((m) => m.name);
      const total = (x: Record<OwnerKind, bigint>) => STACK.reduce((a, k) => a + x[k], 0n);
      // a reset (or a new start) zeroes the counters: the window gets shorter or a sum drops
      if (prev && (snap.elapsedTicks < prev.elapsed || total(sums) < total(prev.sums))) clear();
      if (!prev) {
        marks.push({ seq, text: 'start' });
      } else {
        for (const n of names) if (!prev.names.includes(n)) marks.push({ seq, text: `${shortName(n)} loaded` });
        for (const n of prev.names) if (!names.includes(n)) marks.push({ seq, text: `${shortName(n)} unloaded` });
        const dt = snap.elapsedTicks - prev.elapsed;
        if (dt > 0n) {
          const col = {} as Column;
          for (const k of STACK) col[k] = shareOf(sums[k] - prev.sums[k], dt);
          cols.push({ seq: seq++, col });
          if (cols.length > max) cols.shift();
        }
      }
      prev = { elapsed: snap.elapsedTicks, sums, names };
    },
    columns: () => cols.map((c) => c.col),
    markers() {
      const first = cols[0]?.seq ?? seq;
      return marks.filter((m) => m.seq >= first).map((m) => ({ col: m.seq - first, text: m.text }));
    },
    now() {
      const last = cols[cols.length - 1]?.col;
      return last ? STACK.reduce((a, k) => a + last[k], 0) : 0;
    },
  };
}

/** The top of the graph: the first of 10, 25, 50 and 100% that holds the highest column. */
export function graphTop(cols: Column[]): number {
  const peak = cols.reduce(
    (m, c) =>
      Math.max(
        m,
        STACK.reduce((a, k) => a + c[k], 0),
      ),
    0,
  );
  return [0.1, 0.25, 0.5, 1].find((t) => peak <= t) ?? Math.ceil(peak);
}

// ---- text -----------------------------------------------------------------------------------------------------------------------------

export const fmtShare = (f: number): string => `${(f * 100).toFixed(1)}%`;
export const fmtUs = (us: number): string => us.toFixed(1);
export const fmtMhz = (hz: number): string => `${Number((hz / 1e6).toFixed(2))} MHz`;

/** `mm:ss` of a number of ticks at `hz`. */
export function fmtElapsed(ticks: bigint, hz: number): string {
  const s = hz > 0 ? Math.floor(Number(ticks) / hz) : 0;
  return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
}

// ---- comparison with an exported profile ----------------------------------------------------------------------------------------------

/** The spans of a `dk.profile/1` file, as shares by owner name; `undefined` when the file has no spans or no elapsed window. */
export function readReference(doc: unknown): Map<string, number> | undefined {
  const d = doc as {
    schema?: string;
    spans?: { name?: string; sum_ticks?: number }[];
    span_elapsed_ticks?: number;
  };
  if (!d || d.schema !== 'dk.profile/1') throw new Error('not a dk.profile/1 file');
  if (!d.spans?.length || !d.span_elapsed_ticks || d.span_elapsed_ticks <= 0) return undefined;
  const out = new Map<string, number>();
  for (const s of d.spans) if (s.name) out.set(s.name, (s.sum_ticks ?? 0) / d.span_elapsed_ticks);
  return out;
}

// ---- DSP ------------------------------------------------------------------------------------------------------------------------------

/** One module of the emulator's DSP view: its instructions per block, and the functions that hold most of them. */
export interface DspModule {
  name: string;
  kind: OwnerKind;
  insnPerBlock: number;
  top: { label: string; pct: number }[];
}

/** The emulator's DSP per block: the modules' instructions per DSP block against the device's headroom (CAPACITY DSP_COST). */
export interface DspView {
  blocks: number;
  /** The modules' instructions per block, added up (the stock firmware's DSP work is not in it). */
  total: number;
  /** The device's headroom in instructions per block; undefined when the device does not report it. */
  headroom?: number;
  modules: DspModule[];
}

interface ReportSeries {
  series?: string;
  cpu?: string;
  owners?: { owner: string; per_block?: number }[];
  functions?: { name: string; module?: string; per_block?: number }[];
}

const sameModule = (owner: string, m: ModuleRef) => owner === m.name || shortName(owner) === shortName(m.name);

/**
 * The DSP view from a `dkprof` report of `sharc.profile` (`series` `pc` of the CPU `sharc`): a module's owner row is its instructions per block,
 * and its top functions give the lines under it. Owners that are no loaded module (`runtime`, `firmware`, `unnamed`) are the stock work and stay out.
 */
export function dspView(
  report: { blocks?: number; series?: ReportSeries[] },
  modules: ModuleRef[],
  headroom?: number,
): DspView {
  const s = report.series?.find((x) => x.cpu === 'sharc' && x.series === 'pc');
  const out: DspModule[] = [];
  for (const o of s?.owners ?? []) {
    const m = modules.find((x) => sameModule(o.owner, x));
    if (!m || !o.per_block) continue;
    const top = (s?.functions ?? [])
      .filter((f) => f.module !== undefined && sameModule(f.module, m) && f.per_block)
      .slice(0, 3)
      .map((f) => ({ label: f.name, pct: Math.round((100 * f.per_block!) / o.per_block!) }));
    out.push({ name: m.name, kind: m.kind, insnPerBlock: o.per_block, top });
  }
  out.sort((a, b) => b.insnPerBlock - a.insnPerBlock);
  return {
    blocks: report.blocks ?? 0,
    total: out.reduce((a, m) => a + m.insnPerBlock, 0),
    ...(headroom ? { headroom } : {}),
    modules: out,
  };
}

/** The width of a module's segment in the DSP bar, as a percent of the headroom (or of the total when the device reports none), at most 100. */
export function dspWidth(v: DspView, insn: number): number {
  const whole = v.headroom ?? v.total;
  return whole > 0 ? Math.min(100, (100 * insn) / whole) : 0;
}

/** `43,700 / 92,796`: the total against the headroom, or the total alone. */
export function dspText(v: DspView): string {
  const n = (x: number) => Math.round(x).toLocaleString('en-US');
  return v.headroom ? `${n(v.total)} / ${n(v.headroom)}` : n(v.total);
}
