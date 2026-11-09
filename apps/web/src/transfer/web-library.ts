// The library of the site's Transfer page (pure apart from the two fetch functions it is given; apps/web/tests/web-library.test.ts): the modules the
// site ships (public/modules/index.json, made by scripts/gen-modules.mjs) with their prebuilt .dkm files per device, plus the .dkm files the user adds.
import { hexOf } from '@digi/dk-session';
import { headerInfo } from '../modules/dktransfer';
import type { LibraryFile, TransferLibrary, TransferSource } from './env';

export interface ShippedFile {
  file: string;
  size: number;
  sha256: string;
}
export interface ShippedModule {
  id: string;
  name: string;
  summary: string;
  kind: string;
  version: string;
  devices: Record<string, ShippedFile>;
  /** Source files, relative to the module (`module.toml`, `README.md`, `src/...`). */
  source: string[];
}

export function parseIndex(text: string): ShippedModule[] {
  const j = JSON.parse(text) as { modules?: ShippedModule[] };
  if (!Array.isArray(j.modules)) throw new Error('modules/index.json has no module list');
  return j.modules;
}

export interface Fetchers {
  /** The bytes of a file under the site's modules folder (`dt2/<name>.dkm`, `index.json`). */
  bytes(path: string): Promise<Uint8Array>;
  /** The text of a file under it (`src/<author/name>/src/module.c`). */
  text(path: string): Promise<string>;
}

const USER = 'added/';

export function createWebLibrary(fetchers: Fetchers): { library: TransferLibrary; source: TransferSource } {
  let index: Promise<ShippedModule[]> | undefined;
  const shipped = () => {
    index ??= fetchers.text('index.json').then(parseIndex, (e) => {
      index = undefined; // tried again at the next read
      throw e;
    });
    return index;
  };
  // the modules of the last scan: `files` answers synchronously for the rows that are shown
  let known: ShippedModule[] = [];
  const added = new Map<string, { data: Uint8Array; id: string }>();

  const library: TransferLibrary = {
    async scan(device): Promise<LibraryFile[]> {
      known = await shipped();
      const mine = new Set([...added.values()].map((a) => a.id));
      const out: LibraryFile[] = [];
      for (const m of known) {
        const f = m.devices[device];
        // a file the user added with the same module id replaces the shipped one
        if (f && !mine.has(m.id)) out.push({ path: f.file, size: f.size });
      }
      for (const [name, a] of added) out.push({ path: USER + name, size: a.data.length });
      return out;
    },
    async read(_device, path) {
      if (path.startsWith(USER)) {
        const a = added.get(path.slice(USER.length));
        if (!a) throw new Error(`${path} is not in the list any more`);
        return a.data;
      }
      const f = known.flatMap((m) => Object.values(m.devices)).find((x) => x.file === path);
      const data = await fetchers.bytes(path);
      if (f && hexOf(new Uint8Array(await crypto.subtle.digest('SHA-256', data as BufferSource))) !== f.sha256)
        throw new Error(`${path} does not match modules/index.json: reload the page to get the current files`);
      return data;
    },
    async addFiles(files) {
      const lines: string[] = [];
      for (const f of files) {
        const h = headerInfo(f.data);
        if (!h) throw new Error(`${f.name} is not a .dkm module file.`);
        added.set(f.name, { data: f.data, id: h.name });
        lines.push(`Added ${f.name} (${h.name} ${h.version}) to the list`);
      }
      return lines;
    },
  };

  const source: TransferSource = {
    files: (id) => known.find((m) => m.id === id)?.source ?? [],
    read: (id, path) => fetchers.text(`src/${id}/${path}`),
  };
  return { library, source };
}
