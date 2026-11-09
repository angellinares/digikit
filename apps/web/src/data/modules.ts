// The module gallery, read at build time from the committed modules/**/module.toml and each module's
// gallery/icon.svg (line art on a 48 grid; apps/web/tests/icons.test.mjs checks what an icon may contain).
import { parse } from 'smol-toml';
import type { DeviceId } from './device-profiles';
import { isShipped } from './shipped.mjs';
import type { Category } from './module-labels';

const manifests = import.meta.glob('../../../../modules/**/module.toml', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;
const icons = import.meta.glob('../../../../modules/**/gallery/icon.svg', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

// `patchkit gen` writes build/gen/gallery.json for a module with typed sections ([[machine]], [[param]]): the
// machines it adds and their parameters, per device. Committed; apps/web/tests/gallery.test.mjs checks the shape.
const galleries = import.meta.glob('../../../../modules/**/build/gen/gallery.json', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

export { CATEGORY_LABEL, DEVICE_SHORT, type Category } from './module-labels';

/** A machine a module adds, with the parameters on its knobs. */
export interface GalleryMachine {
  id: string;
  name: string;
  short: string;
  /** Devices it is built for. */
  devices: DeviceId[];
  /** The module's own parameters on the machine's knobs, in knob order (stock parameters are not listed). */
  params: { id: string; name: string; short: string; min: number; max: number; default: number }[];
}

type GalleryFile = {
  machines: {
    id: string;
    name: string;
    short: string;
    devices: Record<string, { knobs: ({ param?: string } | null)[] }>;
  }[];
  params: Record<string, { id: string; name: string; short: string; min: number; max: number; default: number }[]>;
};

export function galleryMachines(text: string | undefined): GalleryMachine[] {
  if (!text) return [];
  const g = JSON.parse(text) as GalleryFile;
  return g.machines.map((m) => {
    const devices = order.filter((d) => d in m.devices);
    const params: GalleryMachine['params'] = [];
    for (const d of devices)
      for (const k of m.devices[d].knobs) {
        const p = k?.param ? g.params[d]?.find((x) => x.id === k.param) : undefined;
        if (p && !params.some((x) => x.id === p.id)) params.push(p);
      }
    return { id: m.id, name: m.name, short: m.short, devices, params };
  });
}

export interface ModuleInfo {
  id: string;
  name: string;
  version: string;
  /** The manifest `author`, empty when absent. */
  author: string;
  /** The module's path in the repository, such as `modules/<kind>/<name>`. */
  dir: string;
  category: Category;
  status: string;
  summary: string;
  requires: string[];
  conflicts: string[];
  /** Resources the module takes (only one module may hold each). */
  claims: string[];
  /** The devices any target names, in a fixed order. */
  devices: DeviceId[];
  /** Inline SVG markup, from the repository. */
  icon: string;
  /** Machines the module adds (empty for the others). */
  machines: GalleryMachine[];
  /** A run-time module (`dynamic = true`): it is loaded from the card by the digikit runtime and cannot be patched into a firmware. */
  dynamic: boolean;
  /** The site ships its prebuilt .dkm files (the Transfer page installs them): see shipped.mjs. */
  shipped: boolean;
}

const FRAMEWORK_ICON = `<svg viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="12" y="12" width="24" height="24" rx="4"/><path d="M18 6v6M24 6v6M30 6v6M18 36v6M24 36v6M30 36v6M6 18h6M6 24h6M6 30h6M36 18h6M36 24h6M36 30h6" stroke-width="1.5"/><rect x="20" y="20" width="8" height="8" rx="1.5" style="stroke: var(--icon-accent); fill: var(--icon-accent)"/></svg>`;

type Manifest = {
  module: {
    id: string;
    name: string;
    version: string;
    author?: string;
    category: Category;
    status: string;
    summary: string;
    dynamic?: boolean;
    type?: string;
    requires?: string[];
    conflicts?: string[];
  };
  targets?: Record<string, string>;
  claim?: { resource: string }[];
};

const order: DeviceId[] = ['dt2', 'dn2'];

export const modules: ModuleInfo[] = Object.entries(manifests)
  .map(([path, text]) => {
    const { module: m, targets = {}, claim = [] } = parse(text) as unknown as Manifest;
    const named = new Set(Object.keys(targets).map((port) => port.split('-')[0]));
    const dir = path.replace(/^(\.\.\/)+/, '').replace(/\/module\.toml$/, '');
    return {
      id: m.id,
      name: m.name,
      version: m.version,
      author: m.author ?? '',
      dir,
      category: m.category,
      status: m.status,
      summary: m.summary,
      requires: m.requires ?? [],
      conflicts: m.conflicts ?? [],
      claims: claim.map((c) => c.resource),
      devices: order.filter((d) => named.has(d)),
      icon: icons[path.replace(/module\.toml$/, 'gallery/icon.svg')] ?? FRAMEWORK_ICON,
      machines: galleryMachines(galleries[path.replace(/module\.toml$/, 'build/gen/gallery.json')]),
      dynamic: m.dynamic ?? false,
      shipped: isShipped(parse(text), dir),
    };
  })
  .sort((a, b) => a.id.localeCompare(b.id));

/** The modules the home page shows and the Transfer page installs: the run-time content modules the site ships. */
const CATEGORY_ORDER: Category[] = ['machines', 'fx', 'tools'];
const categoryRank = (c: Category) => {
  const i = CATEGORY_ORDER.indexOf(c);
  return i < 0 ? CATEGORY_ORDER.length : i;
};
export const shippedModules = modules
  .filter((m) => m.shipped)
  .sort(
    (a, b) =>
      categoryRank(a.category) - categoryRank(b.category) ||
      a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }),
  );
