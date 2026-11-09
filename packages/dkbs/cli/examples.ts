import { readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export interface Example {
  name: string;
  file: string;
  /** Set when the file could not even be imported: it still gets a tile that shows the error. */
  error?: string;
}

/** The `.mjs` files in `dir` that export `ticks` and `draw`. Helpers without those exports are skipped. Only export names are read here; packing runs in a child process. */
export async function discoverExamples(dir: string): Promise<Example[]> {
  const found: Example[] = [];
  for (const entry of readdirSync(dir).sort()) {
    if (!entry.endsWith('.mjs')) continue;
    const file = path.join(dir, entry);
    const name = path.basename(entry, '.mjs');
    try {
      const mod = await import(`${pathToFileURL(file).href}?v=${statSync(file).mtimeMs}`);
      if ('ticks' in mod && 'draw' in mod) found.push({ name, file });
    } catch (e) {
      found.push({ name, file, error: e instanceof Error ? e.message : String(e) });
    }
  }
  return found;
}
