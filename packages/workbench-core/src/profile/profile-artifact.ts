// The `dk.profile/1` artifact (docs/INSTRUMENTATION.md "Profiles"), built from what the sources answer: the runtime spans of the debug module
// (dk-session `profileAll`), `cf.profile` and `sharc.profile` of the emulator. One builder for `wb profile ...` (tools/devtools/wb_prof.mjs) and
// the Profile pane (apps/desktop/ui/src/profile-backend.ts), so both write the same file. No DOM, no Tauri. Host tests: tests/profile-artifact.test.ts.

/** One span of the artifact: the owner id of the runtime, its kind and name, and its counters in ticks of the span clock. */
export interface SpanOut {
  owner: number;
  kind: 'module' | 'phase' | 'poll';
  name: string;
  calls: number;
  sum_ticks: number;
  max_ticks: number;
}

export interface SpansPart {
  spans: SpanOut[];
  span_clock_hz: number;
  /** Ticks of the span clock the counters were gathered over; `sum_ticks / span_elapsed_ticks` is the share of the CPU. */
  span_elapsed_ticks: number;
}

/** What `sharc.profile` answers (docs/CONTROL.md). */
export interface SharcAnswer {
  clock: unknown;
  blocks: number;
  icount: number;
  idle_skipped: number;
  counted: { fast: number; block: number; interpreted: number };
  raw: unknown[];
}

/** What `cf.profile` answers: cumulative tables, so a recording is the difference of two answers. */
export interface CfAnswer {
  pc_samples?: [string, number][];
  idle_pc?: [string, number][];
  exit_pcs?: [string, number][];
  paths: { path: string; instructions?: number; calls?: number; host_ns?: number }[];
}

export interface Metric {
  name: string;
  value?: number;
  unit: string;
  na_reason?: string;
}

/** A part of an artifact that comes from one source. */
export interface Part {
  clock: { source: string; unit: string; sampling: string };
  raw: unknown[];
  metrics: Metric[];
  loss: { series: string; reason: string }[];
  overhead: Metric[];
}

export const CF_PATHS = ['ram_clear', 'run_fast', 'step_once', 'idle_skip'];

/** The spans of `profileAll` (or of an owner table) as the artifact's `spans`. `nameOf` gives each span its name. */
export function spansPart(
  all: {
    hz: number;
    elapsedTicks: bigint;
    spans: { kind: 'module' | 'phase' | 'poll'; index: number; calls: number; sumTicks: bigint; maxTicks: number }[];
  },
  nameOf: (s: { kind: 'module' | 'phase' | 'poll'; index: number }) => string,
): SpansPart {
  return {
    span_clock_hz: all.hz,
    span_elapsed_ticks: Number(all.elapsedTicks),
    spans: all.spans.map((s) => ({
      owner: s.kind === 'module' ? s.index : s.kind === 'phase' ? 0x100 | s.index : 0x200 | s.index,
      kind: s.kind,
      name: nameOf(s),
      calls: Number(s.calls),
      sum_ticks: Number(s.sumTicks),
      max_ticks: Number(s.maxTicks),
    })),
  };
}

/** The DSP counts of `sharc.profile`. */
export function sharcPart(a: SharcAnswer): Part {
  return {
    clock: a.clock as Part['clock'],
    raw: a.raw,
    metrics: [
      { name: 'sharc.blocks', value: a.blocks, unit: 'blocks' },
      { name: 'sharc.insn.executed', value: a.icount, unit: 'insn' },
      { name: 'sharc.insn.idle_skipped', value: a.idle_skipped, unit: 'insn' },
      { name: 'sharc.insn.counted.fast', value: a.counted.fast, unit: 'insn' },
      { name: 'sharc.insn.counted.block', value: a.counted.block, unit: 'insn' },
      { name: 'sharc.insn.counted.interpreted', value: a.counted.interpreted, unit: 'insn' },
      {
        name: 'sharc.coverage',
        unit: 'ratio',
        na_reason:
          'sharc.insn.executed and the per-PC counts do not reconcile in the DN2 proof run (open, profiling plan P4)',
      },
    ],
    loss: [],
    overhead: [
      {
        name: 'sharc-profile.feature',
        unit: 'ratio',
        na_reason: 'not measured: needs a paired run without the feature',
      },
    ],
  };
}

/** Rows `[hex pc, weight]` of two reports as counters of the difference. Rows that fell out of the top-N of the later report are lost. */
export function deltaRows(
  before: [string, number][] | undefined,
  after: [string, number][] | undefined,
  series: string,
): { cpu: 'cf'; addr: number; count: number; series: string }[] {
  const base = new Map((before ?? []).map(([pc, w]) => [pc, w]));
  return (after ?? [])
    .map(([pc, w]) => ({ cpu: 'cf' as const, addr: Number(pc), count: w - (base.get(pc) ?? 0), series }))
    .filter((r) => r.count > 0);
}

/** The ColdFire counts of `cf.profile`: the difference of `after` and `before` (an empty `before` is the state since boot). */
export function cfPart(before: CfAnswer | undefined, after: CfAnswer): Part {
  const b = before ?? { paths: [] };
  const metrics: Metric[] = [];
  for (const p of CF_PATHS) {
    const a = after.paths.find((x) => x.path === p) ?? { path: p };
    const o = b.paths.find((x) => x.path === p) ?? { path: p };
    metrics.push(
      { name: `path.${p}.instructions`, value: (a.instructions ?? 0) - (o.instructions ?? 0), unit: 'insn' },
      { name: `path.${p}.calls`, value: (a.calls ?? 0) - (o.calls ?? 0), unit: 'calls' },
      { name: `path.${p}.host_ns`, value: (a.host_ns ?? 0) - (o.host_ns ?? 0), unit: 'ns' },
    );
  }
  return {
    clock: {
      source: 'emu.cf.insn',
      unit: 'insn',
      sampling:
        'pc: every 64 insns in run_fast, every insn in step_once; idle_pc: weighted by insns skipped; exit_pc: run_fast exits (a count of exits, not insns)',
    },
    raw: [
      ...deltaRows(b.pc_samples, after.pc_samples, 'pc'),
      ...deltaRows(b.idle_pc, after.idle_pc, 'idle_pc'),
      ...deltaRows(b.exit_pcs, after.exit_pcs, 'exit_pc'),
    ],
    metrics,
    loss: [
      {
        series: 'all',
        reason:
          'cf.profile keeps the 400 heaviest rows of its cumulative tables; rows that fell out of the later report are lost from the difference',
      },
    ],
    overhead: [
      {
        name: 'cf-profile.feature',
        unit: 'ratio',
        na_reason: 'not measured: needs a paired run without the feature',
      },
    ],
  };
}

export interface BuildInput {
  run: Record<string, unknown>;
  cf?: Part;
  sharc?: Part;
  spans?: SpansPart;
  /** Names the markers (`record.start` at 0 is always there when a part is given). */
  markers?: { name: string; at: number }[];
}

/**
 * One artifact from the sources that ran. One source gives its clock as it is; with two, the clock names both (the units are the same, instructions).
 * Spans alone have the clock `runtime.span`.
 */
export function buildProfile(i: BuildInput): Record<string, unknown> {
  const parts = [i.cf, i.sharc].filter((p): p is Part => p !== undefined);
  const clock =
    parts.length === 0
      ? {
          source: 'runtime.span',
          unit: 'tick',
          sampling: 'runtime span counters (calls, sum and max of ticks)',
        }
      : parts.length === 1
        ? parts[0]!.clock
        : {
            source: parts.map((p) => p.clock.source).join('+'),
            unit: parts[0]!.clock.unit,
            sampling: parts.map((p) => `${p.clock.source}: ${p.clock.sampling}`).join(' | '),
          };
  const spans = i.spans && i.spans.spans.length > 0 ? i.spans : undefined;
  return {
    schema: 'dk.profile/1',
    run: i.run,
    clock,
    raw: parts.flatMap((p) => p.raw),
    metrics: parts.flatMap((p) => p.metrics),
    markers: i.markers ?? (parts.length > 0 ? [{ name: 'record.start', at: 0 }] : []),
    loss: parts.flatMap((p) => p.loss),
    overhead: parts.flatMap((p) => p.overhead),
    ...(spans ? spans : {}),
  };
}

/** A run id like `emu-20261115T120000`. */
export function runId(prefix: 'emu' | 'dev', now: Date): string {
  return `${prefix}-${now.toISOString().replace(/[-:]/g, '').slice(0, 15)}`;
}
