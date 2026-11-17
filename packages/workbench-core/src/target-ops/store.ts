// Pure helpers for the card store of a target: the categories, the usage by category, the folder tree, the .dkm header and a hex dump.
// No I/O: the entries come from `StoreOps` (ops.ts).
import { HEADER_BYTES, fmtVersion, type DkEntry } from '@digi/dk-session';

export type CategoryKey = 'modules' | 'services' | 'data' | 'samples' | 'other';

/**
 * The categories of the store, in display order. These are the store's own path prefixes (docs/design/digikit-store.md), the same on every device;
 * a device whose samples live elsewhere simply has no `samples/` entries.
 */
export const CATEGORIES: readonly { key: CategoryKey; prefix: string | null; label: string }[] = [
  { key: 'modules', prefix: 'modules/', label: 'Modules' },
  { key: 'services', prefix: 'services/', label: 'Services' },
  { key: 'data', prefix: 'data/', label: 'Module data' },
  { key: 'samples', prefix: 'samples/', label: 'Samples' },
  { key: 'other', prefix: null, label: 'Other' },
];

export function categoryOf(path: string): CategoryKey {
  for (const c of CATEGORIES) if (c.prefix !== null && path.startsWith(c.prefix)) return c.key;
  return 'other';
}

/** An empty folder entry: LIST has no kind field, so a folder is size 0 with an all-zero hash. */
export function isDirEntry(e: DkEntry): boolean {
  return e.size === 0 && /^0*$/.test(e.hash);
}

export interface StoreUsage {
  total: number | null;
  free: number;
  /** In `CATEGORIES` order, only categories with files. */
  used: { key: CategoryKey; label: string; bytes: number; files: number }[];
  /** `total - used - free` (not below 0), or null without a total: the store's table, header copies and settings/selection records. */
  overhead: number | null;
}

export function usageOf(entries: DkEntry[], space: { free: number; total: number | null }): StoreUsage {
  const sums = new Map<CategoryKey, { bytes: number; files: number }>();
  for (const e of entries) {
    if (isDirEntry(e)) continue;
    const key = categoryOf(e.path);
    const s = sums.get(key) ?? { bytes: 0, files: 0 };
    s.bytes += e.size;
    s.files += 1;
    sums.set(key, s);
  }
  const used = CATEGORIES.flatMap((c) => {
    const s = sums.get(c.key);
    return s ? [{ key: c.key, label: c.label, bytes: s.bytes, files: s.files }] : [];
  });
  const sum = used.reduce((a, u) => a + u.bytes, 0);
  return {
    total: space.total,
    free: space.free,
    used,
    overhead: space.total === null ? null : Math.max(0, space.total - sum - space.free),
  };
}

export interface TreeNode {
  name: string;
  /** A folder's path has no trailing '/'. */
  path: string;
  dir: boolean;
  /** A folder: the sum of the file sizes below it. */
  size: number;
  /** A folder: the number of files below it; a file: 1. */
  files: number;
  /** The file's hash slot, hex; null for a folder. */
  hash: string | null;
  children: TreeNode[];
}

const folder = (name: string, path: string): TreeNode => ({
  name,
  path,
  dir: true,
  size: 0,
  files: 0,
  hash: null,
  children: [],
});

/** The tree of the entries. The root has name '' and path ''. Folders are made for every path part and for empty-folder entries. */
export function buildTree(entries: DkEntry[]): TreeNode {
  const root = folder('', '');
  const dirs = new Map<string, TreeNode>([['', root]]);
  const dirAt = (path: string): TreeNode => {
    const have = dirs.get(path);
    if (have) return have;
    const cut = path.lastIndexOf('/');
    const parent = dirAt(cut < 0 ? '' : path.slice(0, cut));
    const node = folder(path.slice(cut + 1), path);
    parent.children.push(node);
    dirs.set(path, node);
    return node;
  };
  for (const e of entries) {
    const path = e.path.replace(/\/+$/, '');
    if (path === '') continue;
    if (isDirEntry(e)) {
      dirAt(path);
      continue;
    }
    const cut = path.lastIndexOf('/');
    dirAt(cut < 0 ? '' : path.slice(0, cut)).children.push({
      name: path.slice(cut + 1),
      path,
      dir: false,
      size: e.size,
      files: 1,
      hash: e.hash,
      children: [],
    });
  }
  const finish = (n: TreeNode): void => {
    n.children.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1));
    for (const c of n.children) {
      if (c.dir) finish(c);
      n.size += c.size;
      n.files += c.files;
    }
  };
  finish(root);
  return root;
}

/** How a module or service file on the store compares with the built modules of the library (matched by store path). */
export type LibraryMark = 'other version' | 'not in library';

/** The mark of each file under `modules/` and `services/`: 'other version' when the library has the path with another hash, 'not in library' when it lacks the path. Files that match get no entry. */
export function libraryMarks(
  entries: DkEntry[],
  library: readonly { path: string; hash: string }[],
): Map<string, LibraryMark> {
  const built = new Map(library.map((l) => [l.path, l.hash]));
  const marks = new Map<string, LibraryMark>();
  for (const e of entries) {
    if (isDirEntry(e)) continue;
    const key = categoryOf(e.path);
    if (key !== 'modules' && key !== 'services') continue;
    const hash = built.get(e.path);
    if (hash === undefined) marks.set(e.path, 'not in library');
    else if (hash !== e.hash) marks.set(e.path, 'other version');
  }
  return marks;
}

/**
 * The store paths under `data/` that no module owns. A module with id `a/b` owns `data/a/b/` (sdk/include/dk/platform/data.h: dk_data_path
 * writes "data/<module_id>/<name>"). Returns the orphan files, plus every folder under `data/` (path without trailing '/') whose files are all
 * orphans. An empty-folder entry is an orphan folder when no id owns it.
 */
export function orphanPaths(entries: DkEntry[], moduleIds: Iterable<string>): Set<string> {
  const owners = Array.from(moduleIds, (id) => `data/${id}/`);
  const owned = (path: string): boolean => owners.some((o) => path.startsWith(o));
  const out = new Set<string>();
  const hasOwned = new Set<string>();
  const ancestors = (path: string): string[] => {
    const list: string[] = [];
    for (let cut = path.lastIndexOf('/'); cut > 'data'.length; cut = path.lastIndexOf('/', cut - 1)) {
      list.push(path.slice(0, cut));
    }
    return list;
  };
  for (const e of entries) {
    if (!e.path.startsWith('data/')) continue;
    const path = e.path.replace(/\/+$/, '');
    if (path === 'data' || path === '') continue;
    if (isDirEntry(e)) {
      if (!owned(`${path}/`)) out.add(path);
      continue;
    }
    if (owned(path)) {
      for (const a of ancestors(path)) hasOwned.add(a);
    } else {
      out.add(path);
      for (const a of ancestors(path)) out.add(a);
    }
  }
  for (const a of hasOwned) out.delete(a);
  return out;
}

export interface TreeRow {
  node: TreeNode;
  depth: number;
}

/** The rows to show, depth first: the root is left out, and the children of a folder follow it only when its path is in `open`. */
export function visibleRows(root: TreeNode, open: ReadonlySet<string>): TreeRow[] {
  const out: TreeRow[] = [];
  const walk = (n: TreeNode, depth: number): void => {
    for (const c of n.children) {
      out.push({ node: c, depth });
      if (c.dir && open.has(c.path)) walk(c, depth + 1);
    }
  };
  walk(root, 0);
  return out;
}

/** The fields of a .dkm or .dks header (docs/design/digikit-dkm.md). */
export interface DkmHeader {
  formatVersion: number;
  fileSize: number;
  headerSize: number;
  /** Lowercase hex of the 32 bytes at 0x10. */
  sha256: string;
  /** The runtime version the file needs, "major.minor.patch" (bit 31, the trace flag, is left out). */
  runtime: string;
  trace: boolean;
  version: string;
  role: 'module' | 'service' | number;
  svcAbi: number;
  name: string;
  imageOff: number;
  imageSize: number;
  bssSize: number;
  align: number;
  relocCount: number;
  importCount: number;
  tableSize: number;
  dspSize: number;
}

const MAGIC = [0x44, 0x4b, 0x4d, 0x46]; // "DKMF"

/** The header of a .dkm; null when the bytes are shorter than the header or do not start with the magic. */
export function parseDkmHeader(b: Uint8Array): DkmHeader | null {
  if (b.length < HEADER_BYTES || MAGIC.some((m, i) => b[i] !== m)) return null;
  const v = new DataView(b.buffer, b.byteOffset, b.length);
  const runtime = v.getUint32(0x30);
  const role = v.getUint32(0x38);
  return {
    formatVersion: v.getUint32(4),
    fileSize: v.getUint32(8),
    headerSize: v.getUint32(0x0c),
    sha256: Array.from(b.subarray(0x10, 0x30), (x) => x.toString(16).padStart(2, '0')).join(''),
    runtime: fmtVersion(runtime & 0x7fffffff),
    trace: (runtime & 0x80000000) !== 0,
    version: fmtVersion(v.getUint32(0x34)),
    role: role === 0 ? 'module' : role === 1 ? 'service' : role,
    svcAbi: v.getUint32(0x3c),
    name: new TextDecoder().decode(b.subarray(0x40, 0x60)).split('\0')[0]!,
    imageOff: v.getUint32(0x60),
    imageSize: v.getUint32(0x64),
    bssSize: v.getUint32(0x68),
    align: v.getUint32(0x6c),
    relocCount: v.getUint32(0x74),
    importCount: v.getUint32(0x7c),
    tableSize: v.getUint32(0x94),
    dspSize: v.getUint32(0x9c),
  };
}

/** Lines of `oooooooo  xx xx ... (16 bytes)  |ascii|`; a byte that is not printable ASCII shows as '.'. */
export function hexDump(bytes: Uint8Array, base = 0): string[] {
  const lines: string[] = [];
  for (let at = 0; at < bytes.length; at += 16) {
    const row = bytes.subarray(at, at + 16);
    const hex = Array.from(row, (x) => x.toString(16).padStart(2, '0')).join(' ');
    const ascii = Array.from(row, (x) => (x >= 0x20 && x < 0x7f ? String.fromCharCode(x) : '.')).join('');
    lines.push(`${(base + at).toString(16).padStart(8, '0')}  ${hex.padEnd(47)}  |${ascii}|`);
  }
  return lines;
}
