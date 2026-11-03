import fs from 'node:fs';
import path from 'node:path';
import { parse } from 'smol-toml';

export interface RuntimeModule {
  id: string;
  /** SDK contract slot the module fills: 1 machine, 2 effect, 3 service. */
  contract: 1 | 2 | 3;
  /** Whether the module carries a DSP image (a dynamic machine or effect). */
  dsp: boolean;
  type: 'machine' | 'fx' | 'service';
  /** Follow the owning module's pack devices; absent or empty means both. */
  ports?: string[];
}

/** The `[module]` table of a module.toml as flat key -> raw value text (only the keys read below). */
function moduleTable(text: string): Map<string, string> {
  const out = new Map<string, string>();
  let inModule = false;
  for (const line of text.split('\n')) {
    const header = /^\s*\[+([^\]]+)\]+/.exec(line);
    if (header) {
      inModule = header[1] === 'module';
      continue;
    }
    const pair = inModule ? /^(\w+)\s*=\s*(.*?)\s*(?:#.*)?$/.exec(line) : null;
    if (pair) out.set(pair[1]!, pair[2]!);
  }
  return out;
}

function readModule(manifest: string, scoped = false): RuntimeModule | undefined {
  const text = fs.readFileSync(manifest, 'utf8');
  // Keep the legacy full scan's eligibility rules; scoped manifests get strict TOML validation up front.
  const legacy = scoped ? undefined : moduleTable(text);
  const table = legacy
    ? {
        id: legacy.get('id')?.replaceAll('"', ''),
        type: legacy.get('type')?.replaceAll('"', ''),
        dynamic: legacy.get('dynamic') === 'true',
        host: legacy.get('host') === 'true',
        ports: undefined,
      }
    : parse(text).module;
  if (!table || typeof table !== 'object' || Array.isArray(table) || table instanceof Date) {
    throw new Error(`${manifest}: missing [module] table`);
  }
  const { id, type, dynamic, host } = table;
  if (!(dynamic === true || host === true || type === 'service')) return;
  if (type !== 'machine' && type !== 'fx' && type !== 'service') return;
  if (typeof id !== 'string' || !/^[^/\s]+\/[^/\s]+$/.test(id)) {
    throw new Error(`${manifest}: invalid module id`);
  }
  const ports = scoped ? table.ports : (parse(text).module as { ports?: unknown } | undefined)?.ports;
  if (ports !== undefined && (!Array.isArray(ports) || !ports.every((port) => typeof port === 'string'))) {
    throw new Error(`${manifest}: ports must be an array of strings`);
  }
  return {
    id,
    type,
    contract: type === 'machine' ? 1 : type === 'fx' ? 2 : 3,
    dsp: dynamic === true && type !== 'service',
    ports: (ports as string[] | undefined) ?? [],
  };
}

/** Production upload candidates, matching isUploadable in tools/nx/modules.mjs. */
export function scanRuntimeModules(root: string): RuntimeModule[] {
  const found: RuntimeModule[] = [];
  for (const kind of fs.readdirSync(root).sort()) {
    const kindDir = path.join(root, kind);
    if (!fs.statSync(kindDir).isDirectory()) continue;
    for (const name of fs.readdirSync(kindDir).sort()) {
      const manifest = path.join(kindDir, name, 'module.toml');
      if (!fs.existsSync(manifest)) continue;
      const module = readModule(manifest);
      if (module) found.push(module);
    }
  }
  return found;
}

export function supportsDevice(module: RuntimeModule, device: string): boolean {
  return !module.ports?.length || module.ports.some((port) => port.startsWith(`${device}-`));
}

/** DIGI_CONFORMANCE_MANIFEST selects one shipped manifest without enumerating other modules.
 * DIGI_CONFORMANCE_MODULES retains the legacy comma-separated ID filter in full-scan mode. */
export function loadRuntimeModules(
  root: string,
  selection: { manifest?: string; ids?: string } = {},
): RuntimeModule[] {
  const ids = (selection.ids ?? '').split(',').map((id) => id.trim()).filter(Boolean);
  let modules: RuntimeModule[];
  if (selection.manifest !== undefined) {
    const manifest = path.resolve(root, selection.manifest);
    const relative = path.relative(fs.realpathSync(root), fs.realpathSync(manifest));
    if (!/^modules[/\\][^/\\]+[/\\][^/\\]+[/\\]module\.toml$/.test(relative)) {
      throw new Error('DIGI_CONFORMANCE_MANIFEST: expected a repository-contained modules/<kind>/<name>/module.toml');
    }
    const module = readModule(manifest, true);
    if (!module) throw new Error(`DIGI_CONFORMANCE_MANIFEST: not a run-time module: ${selection.manifest}`);
    if (ids.length && (ids.length !== 1 || ids[0] !== module.id)) {
      throw new Error(`DIGI_CONFORMANCE_MODULES: selected manifest declares ${module.id}, expected ${ids.join(', ')}`);
    }
    modules = [module];
  } else {
    modules = scanRuntimeModules(path.join(root, 'modules'));
  }
  const unknown = ids.filter((id) => !modules.some((module) => module.id === id));
  if (unknown.length) throw new Error(`DIGI_CONFORMANCE_MODULES: unknown run-time module ${unknown.join(', ')}`);
  return ids.length ? modules.filter((module) => ids.includes(module.id)) : modules;
}
