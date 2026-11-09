// The pure logic of the Memory panel (no DOM, no Tauri): the resources a device limits, one bar each, split into one segment per loaded module.
// Host tests: apps/desktop/ui/tests/memory.test.ts.
//
// The data is CAPACITY (limit and used per resource), MODULES (which modules are loaded) and, per module, what its file takes (`needs_of` of
// packages/dkneeds, through patchkit.wasm: apps/web/src/modules/dkneeds.ts) from the matching file of the local library. What `used` holds beyond
// the known modules (the runtime's own, a module that is not in the library) is one "other" segment. Nothing that is only ticked or pending
// is shown: the bars are what the device holds now.

import { ARENA_HDR } from '@digi/dk-session/gen_proto';
import type { Caps, Needs } from '../../../web/src/modules/fit.ts';

// ARENA_HDR (the arena's block header): the `largest` of the ARENA is a payload, while `free` adds up block sizes. DSP_AREA's `largest` is a raw gap.

/** The type of a module, which sets its colour: the library's groups. A module that is not in the library is `other`. */
export type SegmentKind = 'machine' | 'effect' | 'tool' | 'other';

/** What the panel knows of a library module: its header name (the name MODULES reports), a label, its group and its needs. */
export interface KnownModule {
  name: string;
  label: string;
  group: 'Machines' | 'Effects' | 'Tools';
  needs: Needs | null;
}

export interface Segment {
  /** The module's name, or `other`. */
  id: string;
  label: string;
  kind: SegmentKind;
  amount: number;
}

export interface Bar {
  /** The CAPACITY kind. */
  resource: string;
  /** The name the user sees. */
  label: string;
  /** Bytes (shown in KiB) or a count. */
  bytes: boolean;
  limit: number;
  /** What the device says is used. It can be above `limit`: then `over` is set and the segments add up to `used`. */
  used: number;
  /** One per loaded module that takes some of the resource, then `other` for the rest of `used`. */
  segments: Segment[];
  free: number;
  /** The largest single free block (never above `free`). The fit check needs one block, not the total. */
  largest: number;
  /** Free memory is split: the largest block is smaller than the free total allows (see `ARENA_HDR`). */
  split: boolean;
  /** `used` is above `limit`. */
  over: boolean;
}

interface Spec {
  resource: string;
  label: string;
  bytes: boolean;
  /** What a loaded module takes of the resource. */
  take: (n: Needs) => number;
}

/**
 * The resources the device limits, in the order of the panel. A resource whose limit is 0 or that CAPACITY lacks has no bar (the Digitakt II has
 * no page rows). The arena figure is the module's own block: the block of its relocated DSP image is freed once the transfer to the DSP ends.
 * The voice rows, the selection paths, the effect slots and the staging buffer are not shown (nothing a user can act on).
 */
export const SPECS: Spec[] = [
  { resource: 'DSP_AREA', label: 'DSP memory', bytes: true, take: (n) => n.DSP_AREA ?? 0 },
  { resource: 'ARENA', label: 'CPU memory', bytes: true, take: (n) => n.ARENA ?? 0 },
  { resource: 'MACHINE_ROWS', label: 'Machines', bytes: false, take: (n) => n.MACHINE_ROWS ?? 0 },
  { resource: 'PARAM_ROWS', label: 'Parameters', bytes: false, take: (n) => n.PARAM_ROWS ?? 0 },
  { resource: 'PAGE_ROWS', label: 'Pages', bytes: false, take: (n) => n.PAGE_ROWS ?? 0 },
  { resource: 'MODULES', label: 'Modules', bytes: false, take: () => 1 },
];

const KINDS: Record<KnownModule['group'], SegmentKind> = { Machines: 'machine', Effects: 'effect', Tools: 'tool' };

/** The bars for a device: `caps` is CAPACITY, `loaded` the names MODULES reports (in its order), `library` the local library's modules. */
export function buildBars(caps: Caps, loaded: string[], library: KnownModule[]): Bar[] {
  const bars: Bar[] = [];
  for (const spec of SPECS) {
    const c = caps[spec.resource];
    if (!c || c.limit === 0) continue;
    const used = c.used;
    const free = Math.max(0, c.limit - used);
    const segments: Segment[] = [];
    let known = 0;
    for (const name of loaded) {
      const m = library.find((x) => x.name === name);
      if (!m?.needs) continue;
      const amount = Math.min(spec.take(m.needs), used - known);
      if (amount <= 0) continue;
      segments.push({ id: name, label: m.label, kind: KINDS[m.group] ?? 'other', amount });
      known += amount;
    }
    if (used > known) segments.push({ id: 'other', label: 'Other', kind: 'other', amount: used - known });
    bars.push({
      resource: spec.resource,
      label: spec.label,
      bytes: spec.bytes,
      limit: c.limit,
      used,
      segments,
      free,
      largest: Math.max(0, Math.min(c.largest, free)),
      split: c.largest + (spec.resource === 'ARENA' ? ARENA_HDR : 0) < free,
      over: used > c.limit,
    });
  }
  return bars;
}

/** `259 KiB`: one decimal below 10 KiB, whole KiB above. */
export function kibText(bytes: number): string {
  const k = bytes / 1024;
  return `${k < 10 ? Math.round(k * 10) / 10 : Math.round(k)} KiB`;
}

/** An amount of a bar: KiB for bytes, the number for rows and slots. */
export const amountText = (bar: Pick<Bar, 'bytes'>, n: number): string => (bar.bytes ? kibText(n) : String(n));

/** At the end of a bar: `259 KiB / 512 KiB`, or `2 / 8` for a count. */
export const usedText = (bar: Bar): string => `${amountText(bar, bar.used)} / ${amountText(bar, bar.limit)}`;

/** The hover text of a segment: the module's name and its amount. */
export const segmentTitle = (bar: Bar, s: Segment): string =>
  s.kind === 'other'
    ? `Other: ${amountText(bar, s.amount)} (not from a module of your library)`
    : `${s.label}: ${amountText(bar, s.amount)}`;

/** The free text: the total, and the largest block when memory is split so that it is smaller than the total. */
export const freeText = (bar: Bar): string =>
  bar.split
    ? `Free: ${amountText(bar, bar.free)}, largest block ${amountText(bar, bar.largest)}`
    : `Free: ${amountText(bar, bar.free)}`;

/** The hover text of the free part. */
export const freeTitle = freeText;

/** The line under a bar, if it has something to say: over the limit, or free memory split into smaller blocks. */
export const noteText = (bar: Bar): string | null =>
  bar.over ? `Over the limit by ${amountText(bar, bar.used - bar.limit)}` : bar.split ? freeText(bar) : null;

/** The width of a part of a bar in percent of its limit, or of `used` when that is above the limit (the segments stay proportional). */
export const widthPct = (bar: Bar, n: number): number => {
  const span = Math.max(bar.limit, bar.used);
  return span ? (100 * n) / span : 0;
};
