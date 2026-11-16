import { createHash } from 'node:crypto';

/**
 * Edits of a packed `.dkm` that make the loader or the boot plan refuse it (docs/design/digikit-dkm.md, packages/dkneeds/src/keys.rs). Each edit changes one
 * field of a built fixture; `patched` then writes the file's own sha256 again, so that only the edited field is wrong.
 */

/** Header fields (packages/dkneeds/src/gen_limits.rs). */
export const H = { MAGIC: 0, SHA: 0x10, RUNTIME: 0x30, NAME: 0x40, IMAGE_OFF: 0x60, INIT: 0x80, KEYS_OFF: 0x90, KEYS_SIZE: 0x94 } as const;
/** Bytes of the module name in the header. */
export const NAME_LEN = 32;
/** The tags of the header table's entries. */
export const TAG = { NEEDS: 1, USES: 2, KEYS: 3, KINDS: 4 } as const;

/** The file with its sha256 made right again (the sha field is read as zeros when the hash is made). */
export function rehash(f: Buffer): Buffer {
  f.fill(0, H.SHA, H.SHA + 32);
  createHash('sha256').update(f).digest().copy(f, H.SHA);
  return f;
}

/** A copy of `fixture` with `edit` applied and the sha256 made right again; `name` is the module's name in the header (two modules of one name are E_LOADED). */
export function patched(fixture: Buffer, name: string, edit: (f: Buffer) => void = () => {}): Buffer {
  const f = Buffer.from(fixture);
  f.fill(0, H.NAME, H.NAME + NAME_LEN);
  f.write(name, H.NAME);
  edit(f);
  return rehash(f);
}

/** Where entry `tag` of the header table has its payload, and its item count; throws when the file has no such entry. */
export function entry(f: Buffer, tag: number): { at: number; count: number; bytes: number } {
  const table = f.readUInt32BE(H.KEYS_OFF);
  let at = table + 8;
  for (let i = f.readUInt16BE(table + 6); i > 0; i--) {
    const bytes = f.readUInt32BE(at + 4);
    if (f.readUInt16BE(at) === tag) return { at: at + 8, count: f.readUInt16BE(at + 2), bytes };
    at += 8 + bytes;
  }
  throw new Error(`the header table has no entry with tag ${tag}`);
}

/** The 32-bit FNV-1a hash of a service name (`name_hash` of dkneeds). */
export function nameHash(s: string): number {
  let h = 0x811c9dc5;
  for (const b of Buffer.from(s)) h = Math.imul(h ^ b, 0x01000193) >>> 0;
  return h;
}

/** `moveq #-9,%d0; rts`: the code that makes `init` return `E_INIT` (-9) at once. */
const INIT_FAILS = Buffer.from([0x70, 0xf7, 0x4e, 0x75]);

/**
 * The header table with its NEEDS entry moved behind the others (entries stay in ascending tag order, so it becomes the entry with tag `KINDS`) and
 * rewritten as KINDS items `(kind, length 16, handler offset 8)`. The table keeps its size. The module declares no kind in `init`, so the
 * commit finds a KINDS entry that `init` did not declare.
 */
function needsToKinds(f: Buffer, kinds: number[]): void {
  const table = f.readUInt32BE(H.KEYS_OFF);
  const count = f.readUInt16BE(table + 6);
  const parts: { tag: number; raw: Buffer }[] = [];
  let at = table + 8;
  for (let i = 0; i < count; i++) {
    const bytes = f.readUInt32BE(at + 4);
    parts.push({ tag: f.readUInt16BE(at), raw: Buffer.from(f.subarray(at, at + 8 + bytes)) });
    at += 8 + bytes;
  }
  const needs = parts.find((p) => p.tag === TAG.NEEDS);
  if (!needs || needs.raw.readUInt16BE(2) !== kinds.length) throw new Error(`the fixture needs ${kinds.length} NEEDS items`);
  needs.raw.writeUInt16BE(TAG.KINDS, 0);
  kinds.forEach((kind, k) => {
    needs.raw.writeUInt32BE(kind, 8 + 12 * k);
    needs.raw.writeUInt32BE(16, 12 + 12 * k);
    needs.raw.writeUInt32BE(8, 16 + 12 * k);
  });
  at = table + 8;
  for (const p of [...parts.filter((x) => x !== needs), needs]) at += p.raw.copy(f, at);
}

/** The offset of the count of resource `id` in the USES entry. */
export function usesCountAt(f: Buffer, id: number): number {
  const { at, count } = entry(f, TAG.USES);
  for (let k = 0; k < count; k++) if (f.readUInt32BE(at + 8 * k) === id) return at + 8 * k + 4;
  throw new Error(`no uses entry for resource ${id}`);
}

/** The edits, by the load code each one makes (`E_*`); the comment says which stage gives it. */
export const edits = {
  /** The loader: the magic is wrong (E_FORMAT). */
  badMagic: (f: Buffer) => void f.writeUInt32BE(0, H.MAGIC),
  /** The loader: the module needs the next major version of the runtime (E_CONTRACT, state FAILED). */
  newerRuntime: (f: Buffer) => void f.writeUInt32BE(f.readUInt32BE(H.RUNTIME) + 0x10000, H.RUNTIME),
  /** The plan: a hard need of the runtime's own service at a version the runtime does not have (E_CONTRACT, state REFUSED). */
  needsNewerRuntime: (f: Buffer) => {
    const { at } = entry(f, TAG.NEEDS);
    f.writeUInt32BE(nameHash('runtime'), at);
    f.writeUInt32BE(0x7f0000, at + 4);
    f.writeUInt32BE(1, at + 8);
  },
  /** The plan: a hard need of a service nobody provides (E_MISSING, state REFUSED). */
  needsMissingService: (f: Buffer) => {
    const { at } = entry(f, TAG.NEEDS);
    f.writeUInt32BE(0xdeadbeef, at);
    f.writeUInt32BE(0, at + 4);
    f.writeUInt32BE(1, at + 8);
  },
  /** The plan: keys for record kind `kind`, which no service owns (E_PLAN_KIND). The first group of the KEYS entry changes its kind. */
  unownedKind: (kind: number) => (f: Buffer) => void f.writeUInt32BE(kind, entry(f, TAG.KEYS).at),
  /** The loader: `init` returns -9 at once, so the load fails with E_INIT (state FAILED). The first four bytes of `init` become `moveq #-9,%d0; rts`. */
  failInit: (f: Buffer) => void INIT_FAILS.copy(f, f.readUInt32BE(H.IMAGE_OFF) + f.readUInt32BE(H.INIT)),
  /** The commit: the header table owns record kinds that `init` does not declare (E_DECL_KIND, state FAILED; the plan accepts them: no service owns kinds 91 and 92). */
  undeclaredKinds: (f: Buffer) => needsToKinds(f, [91, 92]),
  /** The plan: the module uses `rows` machine rows, more than the device has (E_PLAN_FIT). */
  machineRows: (resource: number, rows: number) => (f: Buffer) => void f.writeUInt32BE(rows, usesCountAt(f, resource)),
};

/** The file with one bit of its sha256 field changed (E_SHA); the rest is as `patched` leaves it. */
export function unhashed(f: Buffer): Buffer {
  f[H.SHA]! ^= 1;
  return f;
}
