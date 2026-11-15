/**
 * A rig run as input of the workbench profiler (`wb profile report`, packages/dkprof): the per-pc instruction counts of the rig's
 * calls (`profile: true`, crate packages/sharc-rig) as a `dk.profile/1` artifact, and the address map of where the rig placed the
 * linked objects. The rig only produces counters; names, shares and loops come from dkprof (docs/INSTRUMENTATION.md "Profiles").
 * Counts are exact instructions of the generated core, like the `instructions` of a call.
 */
import type { MachineBuilt } from './place.ts';

/** `[pc, count]` per executed pc of one call (`MachineBlock.profile`). */
export type PcCounts = readonly (readonly [pc: number, count: number])[];

/** The counts of several calls added per pc. */
export function sumCounts(calls: readonly PcCounts[]): Map<number, number> {
  const sum = new Map<number, number>();
  for (const counts of calls) for (const [pc, n] of counts) sum.set(pc, (sum.get(pc) ?? 0) + n);
  return sum;
}

export interface RigRun {
  module: string;
  device: 'dt2' | 'dn2';
  /** `group/name` of the conformance case */
  caseName: string;
  /** calls (blocks) the counts were added over */
  blocks: number;
  /** SHA-256 of the rig binary */
  binarySha256?: string;
}

/**
 * The short-word PC where the module's DSP code starts: the rig places the module object after the adapter's, and an analysis of the
 * module's DSP image (`--analyse`) counts its offsets from there.
 */
export const moduleBaseOf = (built: Pick<MachineBuilt, 'moduleCode'>): number => built.moduleCode[0]![0];

/** `dk.profile/1`: raw SHARC counters at the pc of the instruction, the load of the module's DSP image and the run facts. */
export function rigProfile(run: RigRun, counts: ReadonlyMap<number, number>, moduleBase: number): object {
  const raw = [...counts]
    .sort((a, b) => a[0] - b[0])
    .map(([addr, count]) => ({ cpu: 'sharc', addr, count, series: 'pc' }));
  return {
    schema: 'dk.profile/1',
    run: {
      id: `rig:${run.module}:${run.device}:${run.caseName}`,
      target: run.device,
      ...(run.binarySha256 ? { binary_sha256: run.binarySha256 } : {}),
      loads: { modules: [{ id: run.module, base: 0, size: 0, dsp_base: moduleBase }] },
    },
    clock: {
      source: 'sharc-rig.insn',
      unit: 'instructions',
      sampling: 'every instruction of the generated core, per pc; the steady blocks of the case added',
    },
    raw,
    metrics: [{ name: 'sharc.blocks', value: run.blocks, unit: 'blocks' }],
  };
}

/**
 * `dk-addrmap` (packages/dkaddrmap) of a rig placement: every code label of the linked objects. Labels of the module's objects are
 * relative to the start of its DSP image with `module` set (the load of the profile rebases them); the adapter's are absolute. A
 * label's range reaches the next label or the end of its group (the adapter's code, the module's code).
 */
export function rigAddrMap(
  built: Pick<MachineBuilt, 'symbols' | 'moduleCode'>,
  moduleId: string,
  device: string,
): object {
  const base = built.moduleCode[0]![0];
  const moduleEnd = built.moduleCode.at(-1)![1];
  const byAddr = new Map<number, string>();
  for (const [name, at] of Object.entries(built.symbols))
    if (!byAddr.has(at) || name < byAddr.get(at)!) byAddr.set(at, name);
  const sorted = [...byAddr].sort((a, b) => a[0] - b[0]);
  const entries = sorted.map(([at, name], i) => {
    const inModule = at >= base && at < moduleEnd;
    const end = Math.min(sorted[i + 1]?.[0] ?? moduleEnd, at < base ? base : moduleEnd);
    return {
      cpu: 'sharc',
      addr: inModule ? at - base : at,
      size: end - at,
      name,
      layer: 'our',
      ...(inModule ? { module: moduleId } : {}),
    };
  });
  return { format: 'dk-addrmap', version: 1, kind: 'rig', key: { device }, entries };
}
