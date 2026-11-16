// Service files as dependencies (src/modules/services.ts): the NEEDS and PROVIDES readers, the service set of an install and the delete plan.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { HEADER_BYTES, TABLE_MAGIC } from '@digi/dk-session';
import {
  PROVIDER_MAX,
  nameHash,
  planDelete,
  readNeeds,
  readProvides,
  resolveServices,
  serviceFileOf,
  type Need,
  type RuntimeIdentity,
  type ServiceFile,
} from '../src/modules/services.ts';

const v = (major: number, minor: number) => (major << 16) | (minor << 8);
const need = (name: string, min = 0, hard = true): Need => ({ hash32: nameHash(name), minVersion: min, hard });

/** A file with a header, the role word and a header table of the given version behind it. */
function build(opts: {
  version?: number;
  role?: number;
  needs?: Need[];
  provides?: [number, number];
  magic?: number;
  abi?: number;
  builtFor?: number;
}): Uint8Array {
  const entries: number[][] = [];
  const u32 = (n: number) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
  const u16 = (n: number) => [(n >>> 8) & 255, n & 255];
  if (opts.needs)
    entries.push([
      ...u16(1),
      ...u16(opts.needs.length),
      ...u32(12 * opts.needs.length),
      ...opts.needs.flatMap((n) => [...u32(n.hash32), ...u32(n.minVersion), ...u32(n.hard ? 1 : 0)]),
    ]);
  if (opts.provides)
    entries.push([...u16(9), ...u16(1), ...u32(8), ...u32(opts.provides[0]), ...u32(opts.provides[1])]);
  const table = [
    ...u32(opts.magic ?? TABLE_MAGIC),
    ...u16(opts.version ?? 2),
    ...u16(entries.length),
    ...entries.flat(),
  ];
  const f = new Uint8Array(HEADER_BYTES + table.length);
  const d = new DataView(f.buffer);
  d.setUint32(0x30, opts.builtFor ?? 0);
  d.setUint32(0x38, opts.role ?? 0);
  d.setUint32(0x3c, opts.abi ?? 0);
  d.setUint32(0x90, HEADER_BYTES);
  d.setUint32(0x94, table.length);
  f.set(table, HEADER_BYTES);
  return f;
}

function svc(stem: string, version: number, needs: Need[] = [], abi = 0, builtFor = 0): ServiceFile {
  const bytes = build({ role: 1, provides: [nameHash(stem), version], needs, abi, builtFor });
  const s = serviceFileOf(stem, bytes, `h-${stem}`);
  assert.ok(s);
  return s;
}

const RT: RuntimeIdentity = { version: v(0, 9), fingerprint: null };
const RUNTIME = new Set([nameHash('runtime')]);

test('nameHash is FNV-1a 32', () => {
  assert.equal(nameHash(''), 0x811c9dc5);
  assert.equal(nameHash('a'), 0xe40c292c);
  assert.equal(nameHash('foobar'), 0xbf9cf968);
  assert.ok(nameHash('runtime') > 0);
  assert.notEqual(nameHash('fx'), nameHash('machines'));
});

test('readNeeds reads v1 and v2 tables and gives [] without a table', () => {
  const needs = [need('fx', v(1, 2)), need('ui', 0, false)];
  assert.deepEqual(readNeeds(build({ version: 1, needs })), needs);
  assert.deepEqual(readNeeds(build({ version: 2, needs })), needs);
  assert.deepEqual(readNeeds(new Uint8Array(HEADER_BYTES)), []);
  assert.deepEqual(readNeeds(new Uint8Array(4)), []);
});

test('a table with the wrong magic or version is not read', () => {
  const needs = [need('fx')];
  assert.deepEqual(readNeeds(build({ needs, magic: 0x12345678 })), []);
  assert.deepEqual(readNeeds(build({ needs, version: 3 })), []);
  assert.equal(readProvides(build({ provides: [1, 2], magic: 0x12345678 })), null);
});

test('readProvides and serviceFileOf read a service file', () => {
  const bytes = build({ role: 1, provides: [nameHash('fx'), v(1, 3)], needs: [need('runtime')] });
  assert.deepEqual(readProvides(bytes), { hash32: nameHash('fx'), version: v(1, 3) });
  const s = serviceFileOf('fx', bytes, 'abc');
  assert.ok(s);
  assert.equal(s.label, 'fx service');
  assert.equal(s.path, 'services/fx.dks');
  assert.equal(s.file_size, bytes.length);
  assert.equal(s.hash, 'abc');
  assert.deepEqual(s.needs, [need('runtime')]);
});

test('serviceFileOf is null for role 0 and for a file without PROVIDES', () => {
  assert.equal(serviceFileOf('fx', build({ role: 0, provides: [1, 2] }), 'h'), null);
  assert.equal(serviceFileOf('fx', build({ role: 1, needs: [need('ui')] }), 'h'), null);
  assert.equal(readProvides(build({ role: 1 })), null);
});

test('a module that needs only the runtime needs no service', () => {
  const r = resolveServices([{ id: 'a', needs: [need('runtime', v(0, 8))] }], [], RT, RUNTIME);
  assert.deepEqual(r.byModule.get('a'), []);
  assert.deepEqual(r.order, []);
  assert.deepEqual(r.refused, []);
});

test('a module gets the service it needs', () => {
  const fx = svc('fx', v(1, 2));
  const r = resolveServices([{ id: 'a', needs: [need('fx', v(1, 0))] }], [fx], RT, RUNTIME);
  assert.deepEqual(r.byModule.get('a'), [fx]);
  assert.deepEqual(r.order, [fx]);
});

test('a service that needs a service comes after it, and each service is listed once', () => {
  const fx = svc('fx', v(1, 0));
  const machines = svc('machines', v(1, 0), [need('fx', v(1, 0))]);
  const r = resolveServices(
    [
      { id: 'a', needs: [need('machines')] },
      { id: 'b', needs: [need('fx'), need('machines')] },
    ],
    [machines, fx],
    RT,
    RUNTIME,
  );
  assert.deepEqual(r.order, [fx, machines]);
  assert.deepEqual(r.byModule.get('a'), [fx, machines]);
  assert.deepEqual(r.refused, []);
});

test('a hash nobody provides refuses that module only', () => {
  const fx = svc('fx', v(1, 0));
  const r = resolveServices(
    [
      { id: 'a', needs: [need('nothing')] },
      { id: 'b', needs: [need('fx')] },
    ],
    [fx],
    RT,
    RUNTIME,
  );
  assert.equal(r.refused.length, 1);
  assert.equal(r.refused[0]!.module, 'a');
  assert.equal(r.refused[0]!.kind, 'unknown-hash');
  assert.equal(r.byModule.has('a'), false);
  assert.deepEqual(r.byModule.get('b'), [fx]);
  assert.deepEqual(r.order, [fx]);
});

test('an older minor or another major of a service is refused as version', () => {
  const fx = svc('fx', v(1, 2));
  const older = resolveServices([{ id: 'a', needs: [need('fx', v(1, 3))] }], [fx], RT, RUNTIME);
  assert.equal(older.refused[0]!.kind, 'version');
  assert.equal(older.refused[0]!.need, '1.3');
  assert.equal(older.refused[0]!.have, '1.2');
  const other = resolveServices([{ id: 'a', needs: [need('fx', v(2, 0))] }], [fx], RT, RUNTIME);
  assert.equal(other.refused[0]!.kind, 'version');
  const newerMajor = resolveServices([{ id: 'a', needs: [need('fx', v(0, 1))] }], [fx], RT, RUNTIME);
  assert.equal(newerMajor.refused[0]!.kind, 'version');
});

test('a service built for another ABI fingerprint refuses that module only', () => {
  const fx = svc('fx', v(1, 0), [], 0xaaaa, v(0, 5));
  const lfo = svc('lfo', v(1, 0), [], 0xbbbb, v(0, 5));
  const rt: RuntimeIdentity = { version: (0 << 16) | (5 << 8) | 2, fingerprint: 0xbbbb };
  const r = resolveServices(
    [
      { id: 'a', needs: [need('fx')] },
      { id: 'b', needs: [need('lfo')] },
    ],
    [fx, lfo],
    rt,
    RUNTIME,
  );
  assert.deepEqual(r.refused, [
    { module: 'a', service: 'fx', kind: 'fingerprint', runtime: '0.5.2', builtFor: '0.5.0' },
  ]);
  assert.deepEqual(r.order, [lfo]);
});

test('a service without a fingerprint (abi 0, a service in C) is accepted like the loader does', () => {
  const c = svc('svc-test', v(1, 0), [], 0, v(0, 5));
  const rt: RuntimeIdentity = { version: (0 << 16) | (5 << 8) | 2, fingerprint: 0xbbbb };
  const r = resolveServices([{ id: 'a', needs: [need('svc-test')] }], [c], rt, RUNTIME);
  assert.deepEqual(r.refused, []);
  assert.deepEqual(r.order, [c]);
});

test('a runtime that reports no fingerprint accepts every service', () => {
  const fx = svc('fx', v(1, 0), [], 0xaaaa);
  const r = resolveServices([{ id: 'a', needs: [need('fx')] }], [fx], RT, RUNTIME);
  assert.deepEqual(r.refused, []);
});

test('more than PROVIDER_MAX services refuse the modules that push it over', () => {
  const set = Array.from({ length: PROVIDER_MAX + 1 }, (_, i) => svc(`s${i}`, v(1, 0)));
  const mods = set.map((s, i) => ({ id: `m${i}`, needs: [need(s.stem)] }));
  const r = resolveServices(mods, set, RT, RUNTIME);
  assert.equal(r.order.length, PROVIDER_MAX);
  assert.deepEqual(
    r.refused.map((x) => [x.module, x.kind]),
    [[`m${PROVIDER_MAX}`, 'too-many']],
  );
  assert.equal(r.byModule.size, PROVIDER_MAX);
});

const card = (...e: [string, Need[]][]) => e.map(([path, needs]) => ({ path, needs }));

test('deleting the only module of a service deletes the service too', () => {
  const c = card(['modules/a.dkm', [need('fx')]], ['modules/b.dkm', []], ['services/fx.dks', []]);
  const p = planDelete(c, ['modules/a.dkm']);
  assert.deepEqual(p.delete.sort(), ['modules/a.dkm', 'services/fx.dks']);
  assert.deepEqual(p.kept, []);
  assert.deepEqual(p.refused, []);
});

test('a service another module still needs is kept', () => {
  const c = card(['modules/a.dkm', [need('fx')]], ['modules/b.dkm', [need('fx')]], ['services/fx.dks', []]);
  const p = planDelete(c, ['modules/a.dkm']);
  assert.deepEqual(p.delete, ['modules/a.dkm']);
  assert.deepEqual(p.kept, [{ path: 'services/fx.dks', by: ['modules/b.dkm'] }]);
  assert.deepEqual(p.refused, []);
});

test('deleting a needed service alone is refused', () => {
  const c = card(['modules/a.dkm', [need('fx')]], ['services/fx.dks', []]);
  const p = planDelete(c, ['services/fx.dks']);
  assert.deepEqual(p.delete, []);
  assert.deepEqual(p.refused, [{ path: 'services/fx.dks', by: ['modules/a.dkm'] }]);
});

test('deleting a module and its service together deletes both', () => {
  const c = card(['modules/a.dkm', [need('fx')]], ['services/fx.dks', []]);
  const p = planDelete(c, ['modules/a.dkm', 'services/fx.dks']);
  assert.deepEqual(p.delete.sort(), ['modules/a.dkm', 'services/fx.dks']);
  assert.deepEqual(p.refused, []);
  assert.deepEqual(p.kept, []);
});
