// Services (.dks files) as dependencies of modules (.dkm): read the NEEDS and PROVIDES of a header table, pick the services an
// install needs (dependencies first), refuse a module whose services do not fit, and plan which files a delete takes with it.
// Pure: no I/O. The runtime fingerprint and the binding of a service to the firmware arrive through seams (RuntimeIdentity, ServiceBinder).
import { HEADER_BYTES, TABLE_MAGIC, fmtVersion } from '@digi/dk-session';
import type { Needs } from './fit.ts';

/** The most services one install set may hold. */
export const PROVIDER_MAX = 6;

const NEEDS_TAG = 1;
const PROVIDES_TAG = 9;
const ROLE_SERVICE = 1;

export interface Need {
  hash32: number;
  /** Packed major<<16|minor<<8|patch. */
  minVersion: number;
  hard: boolean;
}

export interface ServiceFile {
  stem: string;
  label: string;
  hash32: number;
  version: number;
  /** The ABI fingerprint the service was built for (header word 0x3c); it must equal the runtime's `svc_abi`; 0 (a service in C, no `dk_svc_abi`) is accepted as the loader does. */
  abi: number;
  /** The runtime version the file was built for (header word 0x30 without the trace bit 31), for the refusal text. */
  builtFor: number;
  path: string;
  bytes: Uint8Array;
  file_size: number;
  hash: string;
  needs: Need[];
}

export interface RuntimeIdentity {
  version: number;
  /** The runtime's service ABI fingerprint (INFO `svc_abi`); null when the runtime does not report it (not checked). */
  fingerprint: number | null;
}

export interface ServiceBinder {
  bind(dks: Uint8Array, firmwareHash: string): Promise<Uint8Array>;
}

export type ServiceRefusalKind = 'missing' | 'version' | 'fingerprint' | 'unknown-hash' | 'too-many' | 'no-stock';

export interface ServiceRefusal {
  module: string;
  service: string;
  kind: ServiceRefusalKind;
  need?: string;
  have?: string;
  builtFor?: string;
  runtime?: string;
}

export type ServiceState = 'not on device' | 'on device' | 'differs';

export interface ServiceRef {
  stem: string;
  label: string;
  version: number;
  state: ServiceState;
  size: number;
  /** What a load of the bound service file takes of the device (the same capacity figures as a module); the fit of a ticked set adds it once. */
  needs?: Needs;
}

/** FNV-1a 32 of the name (packages/dkneeds/src/keys.rs name_hash). */
export function nameHash(s: string): number {
  let h = 0x811c9dc5;
  for (const b of new TextEncoder().encode(s)) {
    h ^= b;
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

/** The items of one tag of the header table (header 0x90/0x94 pair); [] without a v1 or v2 table. */
function tableItems(f: Uint8Array, tag: number, itemBytes: number): DataView[] {
  if (f.length < HEADER_BYTES) return [];
  const v = new DataView(f.buffer, f.byteOffset, f.length);
  const off = v.getUint32(0x90);
  const size = v.getUint32(0x94);
  if (size < 8 || off + size > f.length || v.getUint32(off) !== TABLE_MAGIC) return [];
  const version = v.getUint16(off + 4);
  if (version < 1 || version > 2) return [];
  const out: DataView[] = [];
  let at = off + 8;
  for (let i = v.getUint16(off + 6); i > 0 && at + 8 <= off + size; i--) {
    const t = v.getUint16(at);
    const count = v.getUint16(at + 2);
    const len = v.getUint32(at + 4);
    if (t === tag) {
      for (let j = 0; j < count && (j + 1) * itemBytes <= len && at + 8 + (j + 1) * itemBytes <= off + size; j++)
        out.push(new DataView(f.buffer, f.byteOffset + at + 8 + j * itemBytes, itemBytes));
    }
    at += 8 + len;
  }
  return out;
}

/** The NEEDS of a file (tag 1, items of hash, min version, flags with bit 0 = hard); [] when there is no table. */
export function readNeeds(bytes: Uint8Array): Need[] {
  return tableItems(bytes, NEEDS_TAG, 12).map((d) => ({
    hash32: d.getUint32(0),
    minVersion: d.getUint32(4),
    hard: (d.getUint32(8) & 1) !== 0,
  }));
}

/** The PROVIDES item of a service file (tag 9: hash, version); null when there is none. */
export function readProvides(bytes: Uint8Array): { hash32: number; version: number } | null {
  const [d] = tableItems(bytes, PROVIDES_TAG, 8);
  return d ? { hash32: d.getUint32(0), version: d.getUint32(4) } : null;
}

/** The service a .dks file holds: header role word 1 (u32 BE at 0x38) and a PROVIDES item; null otherwise. */
export function serviceFileOf(stem: string, bytes: Uint8Array, hash: string): ServiceFile | null {
  if (bytes.length < HEADER_BYTES) return null;
  const v = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
  if (v.getUint32(0x38) !== ROLE_SERVICE) return null;
  const p = readProvides(bytes);
  if (!p) return null;
  return {
    stem,
    label: `${stem} service`,
    hash32: p.hash32,
    version: p.version,
    abi: v.getUint32(0x3c),
    builtFor: v.getUint32(0x30) & 0x7fffffff,
    path: `services/${stem}.dks`,
    bytes,
    file_size: bytes.length,
    hash,
    needs: readNeeds(bytes),
  };
}

function fmt(v: number): string {
  return `${(v >>> 16) & 0xff}.${(v >>> 8) & 0xff}`;
}

/** Same major, minor not older; a need with no version (0) takes any. */
function versionFits(have: number, min: number): boolean {
  return min === 0 || (have >>> 16 === min >>> 16 && have >>> 0 >= min >>> 0);
}

function hex(h: number): string {
  return `0x${(h >>> 0).toString(16).padStart(8, '0')}`;
}

/** The services each module needs (dependencies first), the install order of all of them, and the refusals.
 * A refused module is not in `byModule` and adds nothing to `order`; the others still resolve. */
export interface ResolvedServices {
  byModule: Map<string, ServiceFile[]>;
  order: ServiceFile[];
  refused: ServiceRefusal[];
}

export function resolveServices(
  mods: { id: string; needs: Need[] }[],
  set: ServiceFile[],
  rt: RuntimeIdentity,
  runtimeServices: Set<number>,
): ResolvedServices {
  const provider = new Map<number, ServiceFile>();
  for (const s of set) {
    const o = provider.get(s.hash32);
    if (!o || s.version > o.version) provider.set(s.hash32, s);
  }
  const byModule = new Map<string, ServiceFile[]>();
  const order: ServiceFile[] = [];
  const refused: ServiceRefusal[] = [];
  const chosen = new Set<ServiceFile>();

  // The closure of one module in dependency order, or the refusal.
  const closure = (id: string, needs: Need[]): ServiceFile[] | ServiceRefusal => {
    const out: ServiceFile[] = [];
    const seen = new Set<ServiceFile>();
    const visit = (ns: Need[]): ServiceRefusal | null => {
      for (const n of ns) {
        if (!n.hard) continue;
        if (runtimeServices.has(n.hash32)) {
          if (!versionFits(rt.version, n.minVersion))
            return { module: id, service: 'runtime', kind: 'version', need: fmt(n.minVersion), have: fmt(rt.version) };
          continue;
        }
        const s = provider.get(n.hash32);
        if (!s) return { module: id, service: hex(n.hash32), kind: 'unknown-hash' };
        if (!versionFits(s.version, n.minVersion))
          return { module: id, service: s.stem, kind: 'version', need: fmt(n.minVersion), have: fmt(s.version) };
        // abi 0: the file defines no `dk_svc_abi` (a service in C); the runtime's loader accepts it too (RealRegistry::abi_ok)
        if (rt.fingerprint !== null && s.abi !== 0 && s.abi !== rt.fingerprint)
          return {
            module: id,
            service: s.stem,
            kind: 'fingerprint',
            runtime: fmtVersion(rt.version),
            builtFor: fmtVersion(s.builtFor),
          };
        if (seen.has(s)) continue;
        seen.add(s);
        const r = visit(s.needs);
        if (r) return r;
        out.push(s);
      }
      return null;
    };
    return visit(needs) ?? out;
  };

  for (const m of mods) {
    const r = closure(m.id, m.needs);
    if (!Array.isArray(r)) {
      refused.push(r);
      continue;
    }
    const union = new Set([...chosen, ...r]);
    if (union.size > PROVIDER_MAX) {
      const extra = r.find((s) => !chosen.has(s))!;
      refused.push({ module: m.id, service: extra.stem, kind: 'too-many' });
      continue;
    }
    for (const s of r)
      if (!chosen.has(s)) {
        chosen.add(s);
        order.push(s);
      }
    byModule.set(m.id, r);
  }
  return { byModule, order, refused };
}

/** The files a delete takes: each .dks no remaining .dkm hard-needs goes with the modules that used it; a .dks that is still needed
 * is kept (named with the modules that need it), and deleting a needed .dks alone is refused. A service hash is `nameHash(stem)`. */
export function planDelete(
  card: { path: string; needs: Need[] }[],
  paths: string[],
): {
  delete: string[];
  kept: { path: string; by: string[] }[];
  refused: { path: string; by: string[] }[];
} {
  const chosen = new Set(paths);
  const isService = (p: string) => p.endsWith('.dks');
  const stemOf = (p: string) => p.replace(/^.*\//, '').replace(/\.dks$/, '');
  const services = new Map<number, { path: string; needs: Need[] }>();
  for (const c of card) if (isService(c.path)) services.set(nameHash(stemOf(c.path)), c);

  // The .dks paths a file reaches through hard needs.
  const reach = (needs: Need[]): Set<string> => {
    const out = new Set<string>();
    const walk = (ns: Need[]) => {
      for (const n of ns) {
        const s = n.hard ? services.get(n.hash32) : undefined;
        if (!s || out.has(s.path)) continue;
        out.add(s.path);
        walk(s.needs);
      }
    };
    walk(needs);
    return out;
  };

  const modules = card.filter((c) => !isService(c.path));
  const remaining = modules.filter((m) => !chosen.has(m.path));
  const deleted = modules.filter((m) => chosen.has(m.path));
  const usedBy = new Map<string, string[]>();
  for (const m of remaining) for (const p of reach(m.needs)) usedBy.set(p, [...(usedBy.get(p) ?? []), m.path]);

  const del = new Set<string>(paths.filter((p) => !isService(p)));
  const kept: { path: string; by: string[] }[] = [];
  const refused: { path: string; by: string[] }[] = [];
  const freed = new Set<string>();
  for (const m of deleted) for (const p of reach(m.needs)) freed.add(p);
  for (const p of paths) if (isService(p)) freed.add(p);
  for (const p of freed) {
    const by = usedBy.get(p);
    if (!by) del.add(p);
    else if (chosen.has(p)) refused.push({ path: p, by });
    else kept.push({ path: p, by });
  }
  return { delete: paths.filter((p) => del.has(p)).concat([...del].filter((p) => !chosen.has(p))), kept, refused };
}
