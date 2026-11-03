import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { loadRuntimeModules, supportsDevice } from '../../conformance/manifests.ts';

let root: string;
beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'conformance-manifests-'));
});
afterEach(() => {
  vi.restoreAllMocks();
  fs.rmSync(root, { recursive: true, force: true });
});

function manifest(name: string, body = 'type = "machine"\ndynamic = true', directory = name): string {
  const file = path.join(root, 'modules', 'machines', directory, 'module.toml');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `[module]\nid = "test/${name}"\n${body}\n`);
  return file;
}

test('scoped selection reads only its manifest and does not enumerate the module tree', () => {
  const selected = manifest('own');
  const other = manifest('other');
  fs.writeFileSync(other, 'invalid TOML [');
  const read = vi.spyOn(fs, 'readFileSync');
  const scan = vi.spyOn(fs, 'readdirSync');
  const modules = loadRuntimeModules(root, { manifest: path.relative(root, selected), ids: 'test/own' });
  expect(modules).toEqual([{ id: 'test/own', type: 'machine', contract: 1, dsp: true, ports: [] }]);
  expect(read.mock.calls.map(([file]) => file)).toEqual([selected]);
  expect(scan).not.toHaveBeenCalled();
});

test('full selection and legacy ID filtering retain all runtime types and DSP modes', () => {
  manifest('machine');
  manifest('effect', 'type = "fx"\ndynamic = true');
  manifest('service', 'type = "service"');
  manifest('host', 'type = "machine"\nhost = true');
  manifest('static', 'type = "machine"');
  const modules = loadRuntimeModules(root);
  expect(modules.map(({ id, contract, dsp }) => [id, contract, dsp])).toEqual([
    ['test/effect', 2, true],
    ['test/host', 1, false],
    ['test/machine', 1, true],
    ['test/service', 3, false],
  ]);
  expect(loadRuntimeModules(root, { ids: 'test/service' })).toEqual([modules[3]]);
  expect(() => loadRuntimeModules(root, { ids: 'test/unknown' })).toThrow('unknown run-time module');
});

test('full selection filters one-device modules while retaining modules with absent or empty ports', () => {
  manifest('dn2', 'type = "service"\nports = ["dn2-1.12"]');
  manifest('dt2', 'type = "machine"\ndynamic = true\nports = ["dt2-1.17"]');
  manifest('both');
  manifest('empty', 'type = "service"\nports = []');
  const modules = loadRuntimeModules(root);
  expect(modules.filter((module) => supportsDevice(module, 'dt2')).map((module) => module.id)).toEqual([
    'test/both',
    'test/dt2',
    'test/empty',
  ]);
  expect(modules.filter((module) => supportsDevice(module, 'dn2')).map((module) => module.id)).toEqual([
    'test/both',
    'test/dn2',
    'test/empty',
  ]);
});

test('scoped selection derives its ID and follows declared pack devices', () => {
  const selected = manifest('own', 'type = "service"\nports = ["dn2-1.12"]');
  const [module] = loadRuntimeModules(root, { manifest: selected });
  expect(module).toMatchObject({ id: 'test/own', contract: 3, dsp: false });
  expect(supportsDevice(module!, 'dn2')).toBe(true);
  expect(supportsDevice(module!, 'dt2')).toBe(false);
  expect(supportsDevice({ ...module!, ports: [] }, 'dt2')).toBe(true);
  expect(() => loadRuntimeModules(root, { manifest: selected, ids: 'test/other' })).toThrow(
    'selected manifest declares',
  );
  expect(() => loadRuntimeModules(root, { manifest: selected, ids: 'test/own,test/other' })).toThrow(
    'selected manifest declares',
  );
});

test('scoped selection rejects missing, outside and symlink-escaped paths', () => {
  expect(() => loadRuntimeModules(root, { manifest: 'modules/missing/module.toml' })).toThrow();
  const outside = path.join(root, 'outside.toml');
  fs.writeFileSync(outside, '[module]\nid = "test/outside"\ntype = "service"');
  expect(() => loadRuntimeModules(root, { manifest: outside })).toThrow('repository-contained');
  const selected = manifest('own');
  fs.unlinkSync(selected);
  fs.symlinkSync(outside, selected);
  expect(() => loadRuntimeModules(root, { manifest: selected })).toThrow('repository-contained');
});

test.each([
  ['type = "machine"', 'not a run-time module'],
  ['type = "other"\nhost = true', 'not a run-time module'],
  ['type = "service"\nports = "dn2-1.12"', 'ports must be an array'],
  ['type = "service"\nports = [1]', 'ports must be an array'],
])('scoped selection rejects an invalid runtime contract: %s', (body, error) => {
  const selected = manifest('own', body);
  expect(() => loadRuntimeModules(root, { manifest: selected })).toThrow(error);
});

test('scoped selection rejects malformed TOML and invalid IDs', () => {
  const selected = manifest('own');
  fs.writeFileSync(selected, '[module]\nid = "invalid"\ntype = "service"');
  expect(() => loadRuntimeModules(root, { manifest: selected })).toThrow('invalid module id');
  fs.writeFileSync(selected, 'invalid TOML [');
  expect(() => loadRuntimeModules(root, { manifest: selected })).toThrow();
});
