/**
 * Per-pc instruction counts of one rig call (`profile: true`, crate packages/sharc-rig) as tables: the totals per symbol of the
 * linked objects and the runs of instructions that executed equally often (a loop body, a straight stretch). Counts are exact
 * instructions of the generated core, like the `instructions` of a call; a symbol is every label of the placed objects, so the
 * table follows the assembler's labels, not the source's functions.
 */
export type PcCounts = readonly (readonly [pc: number, count: number])[];

export interface SymbolRow {
  symbol: string;
  /** instructions executed in the symbol's range (from its label to the next label) */
  count: number;
  /** distinct pcs executed in it */
  pcs: number;
  percent: number;
}

export interface RunRow {
  /** `symbol` or `symbol+0xN` (parcels from the label) of the first pc */
  from: string;
  first: number;
  last: number;
  /** executions of each instruction of the run (loop passes) */
  each: number;
  /** instructions in the run */
  length: number;
  count: number;
  percent: number;
}

export const totalOf = (counts: PcCounts): number => counts.reduce((s, [, n]) => s + n, 0);

/** The labels sorted by address, aliases of one address keeping the first name. */
function ordered(symbols: Record<string, number>): [number, string][] {
  const byAddr = new Map<number, string>();
  for (const [name, at] of Object.entries(symbols)) if (!byAddr.has(at) || name < byAddr.get(at)!) byAddr.set(at, name);
  return [...byAddr].sort((a, b) => a[0] - b[0]);
}

/** The label at or before `pc` (binary search): its name and address; `?` before the first label. */
function labelOf(sorted: readonly [number, string][], pc: number): [string, number] {
  let lo = 0;
  let hi = sorted.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (sorted[mid]![0] <= pc) lo = mid + 1;
    else hi = mid;
  }
  const hit = sorted[lo - 1];
  return hit ? [hit[1], hit[0]] : ['?', pc];
}

/** Totals per symbol, largest first, with the share of the call's total. */
export function profileBySymbol(counts: PcCounts, symbols: Record<string, number>): SymbolRow[] {
  const sorted = ordered(symbols);
  const total = totalOf(counts);
  const rows = new Map<string, SymbolRow>();
  for (const [pc, n] of counts) {
    const [name] = labelOf(sorted, pc);
    const row = rows.get(name) ?? { symbol: name, count: 0, pcs: 0, percent: 0 };
    row.count += n;
    row.pcs += 1;
    rows.set(name, row);
  }
  const out = [...rows.values()].sort((a, b) => b.count - a.count || a.symbol.localeCompare(b.symbol));
  for (const r of out) r.percent = total ? (100 * r.count) / total : 0;
  return out;
}

/**
 * Runs of neighbouring executed pcs with the same count, largest total first. An instruction is one or two parcels, X * apart are neighbours; a label starts a new run. A loop of N passes shows as a run with `each` N, straight code that runs once per call as `each` 1.
 */
export function profileRuns(counts: PcCounts, symbols: Record<string, number>): RunRow[] {
  const sorted = ordered(symbols);
  const total = totalOf(counts);
  const out: RunRow[] = [];
  let run: RunRow | null = null;
  for (const [pc, n] of counts) {
    const [name, at] = labelOf(sorted, pc);
    if (run && n === run.each && pc - run.last <= 3 && pc !== at) {
      run.last = pc;
      run.length += 1;
      run.count += n;
      continue;
    }
    if (run) out.push(run);
    run = {
      from: pc === at ? name : `${name}+0x${(pc - at).toString(16)}`,
      first: pc,
      last: pc,
      each: n,
      length: 1,
      count: n,
      percent: 0,
    };
  }
  if (run) out.push(run);
  for (const r of out) r.percent = total ? (100 * r.count) / total : 0;
  return out.sort((a, b) => b.count - a.count || a.first - b.first);
}

const pad = (v: string | number, w: number): string => String(v).padStart(w);

export function formatSymbols(rows: readonly SymbolRow[], total: number, top = 20): string {
  const lines = [`instructions ${total}`, `${pad('insn', 8)} ${pad('%', 6)} ${pad('pcs', 5)}  symbol`];
  for (const r of rows.slice(0, top))
    lines.push(`${pad(r.count, 8)} ${pad(r.percent.toFixed(1), 6)} ${pad(r.pcs, 5)}  ${r.symbol}`);
  if (rows.length > top) {
    const rest = rows.slice(top);
    lines.push(
      `${pad(
        rest.reduce((s, r) => s + r.count, 0),
        8,
      )} ${pad('', 6)} ${pad('', 5)}  (${rest.length} more)`,
    );
  }
  return lines.join('\n');
}

export function formatRuns(rows: readonly RunRow[], top = 20): string {
  const lines = [`${pad('insn', 8)} ${pad('%', 6)} ${pad('passes', 6)} ${pad('len', 4)}  from`];
  for (const r of rows.slice(0, top))
    lines.push(`${pad(r.count, 8)} ${pad(r.percent.toFixed(1), 6)} ${pad(r.each, 6)} ${pad(r.length, 4)}  ${r.from}`);
  return lines.join('\n');
}
