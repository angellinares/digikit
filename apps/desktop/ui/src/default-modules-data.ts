/// <reference types="vite/client" />
// The data side of the desktop's default modules: the list from the device files (`[digikit] default_modules`, with the one-line summary of each
// module's own module.toml) and the wait for the runtime. The install itself is in default-modules.ts. Host tests: apps/desktop/ui/tests/default-modules.test.ts.
import { parse } from 'smol-toml';
import { DkError, type DkSession } from '@digi/dk-session';
import dt2Source from '../../../../devices/digitakt-ii.toml?raw';
import dn2Source from '../../../../devices/digitone-ii.toml?raw';
import type { DefaultModuleEntry } from '../../../web/src/firmware/default-modules-core';

const sources: Record<string, string> = { dt2: dt2Source, dn2: dn2Source };

// the one-line summary of a module is in its own module.toml
const manifests = import.meta.glob('../../../../modules/*/*/module.toml', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** The `summary` of the module with this id in a set of module.toml texts, or ''. */
export function summaryOf(texts: string[], id: string): string {
  for (const text of texts) {
    const m = (parse(text) as { module?: { id?: string; summary?: string } }).module;
    if (m?.id === id) return m.summary ?? '';
  }
  return '';
}

/** `[digikit] default_modules` of a device file's text. */
export function defaultModuleIds(source: string): string[] {
  const ids = (parse(source) as { digikit?: { default_modules?: unknown } }).digikit?.default_modules;
  if (ids === undefined) return [];
  if (!Array.isArray(ids) || ids.some((id) => typeof id !== 'string'))
    throw new Error('a device file has a [digikit] default_modules that is not a list of module ids');
  return ids as string[];
}

/** The list for a device id; a module the repository does not know is listed with no summary (the install says that it has no .dkm). */
export function entries(device: string): DefaultModuleEntry[] {
  const source = sources[device];
  if (!source) return [];
  return defaultModuleIds(source).map((id) => ({ id, summary: summaryOf(Object.values(manifests), id) }));
}

/** Ask INFO until the runtime answers (a booting emulator or a device that just flashed does not yet). */
export async function waitForRuntime(
  s: Pick<DkSession, 'info'>,
  limits = { ms: 60_000, everyMs: 1000 },
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  now: () => number = Date.now,
): Promise<void> {
  const end = now() + limits.ms;
  let last = 'no answer';
  for (;;) {
    try {
      const info = await s.info(true);
      if (info.runtime >= 4) return;
      last = 'the firmware answers but has no digikit runtime';
    } catch (e) {
      if (e instanceof DkError && e.kind === 'cancelled') throw e;
      last = e instanceof Error ? e.message : String(e);
    }
    if (now() >= end)
      throw new Error(`the runtime does not answer INFO within ${Math.round(limits.ms / 1000)} s (${last})`);
    await sleep(limits.everyMs);
  }
}
