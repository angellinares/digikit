import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { deviceTargets, fileDevices } from '../device-tests.mjs';

const none = () => [];
const halves = (tests: string[], recs: object[], opts: object = {}) =>
  Object.keys(deviceTargets('test-device', 'p', tests, recs, null, none, none, false, opts)).sort();

it('gives a file pinned to one device only that half', () => {
  const recs = [{ files: ['a.device.test.ts'], devices: ['dt2'] }, { files: ['b.device.test.ts'] }];
  const names = halves(['a.device.test.ts', 'b.device.test.ts'], recs);
  expect(names).toContain('test-device--a--dt2');
  expect(names).not.toContain('test-device--a--dn2');
  expect(names).toEqual(expect.arrayContaining(['test-device--b--dt2', 'test-device--b--dn2']));
});

it('keeps both halves, and the names, of a file that runs on both devices', () => {
  expect(halves(['a.device.test.ts', 'b.device.test.ts'], [])).toEqual([
    'test-device',
    'test-device--a',
    'test-device--a--dn2',
    'test-device--a--dt2',
    'test-device--b',
    'test-device--b--dn2',
    'test-device--b--dt2',
  ]);
});

it('makes the parent depend on the halves that exist, also for a single-file project and the full-tier twin', () => {
  const targets = deviceTargets(
    'test-device',
    'p',
    ['a.device.test.ts'],
    [{ files: ['a.device.test.ts'], devices: ['dn2'] }],
    null,
    none,
    none,
    false,
    { twin: true },
  );
  expect(Object.keys(targets).sort()).toEqual([
    'test-device',
    'test-device--dn2',
    'test-device-all',
    'test-device-all--dn2',
  ]);
  expect(targets['test-device'].dependsOn).toEqual(['test-device--dn2']);
  expect(targets['test-device-all--dn2'].options.env.DIGI_DEVICE).toBe('dn2');
});

it('takes the devices of a single-device project for files that no record pins', () => {
  const names = halves(['a.device.test.ts', 'b.device.test.ts'], [], { projectDevices: ['dt2'] });
  expect(names.filter((n) => n.endsWith('--dn2'))).toEqual([]);
  expect(names).toContain('test-device--a--dt2');
  // a record's own list wins over the project
  expect(fileDevices('a.device.test.ts', [{ files: ['a.device.test.ts'], devices: ['dn2'] }], ['dt2'])).toEqual([
    'dn2',
  ]);
});

it('rejects a devices list with an unknown device or none', () => {
  expect(() => fileDevices('a', [{ files: ['a'], devices: ['dt3'] }])).toThrow(/dt2 and dn2/);
  expect(() => fileDevices('a', [{ files: ['a'], devices: [] }])).toThrow(/dt2 and dn2/);
});

// The declared devices must agree with what the test source pins: a file that the source pins to one device and the record leaves
// on both would queue an empty half, and a record that drops a device the source runs would lose that coverage.
it('declares in the records the device pins of the test sources', () => {
  const root = join(__dirname, '../../..');
  const files = execFileSync('git', ['ls-files', '*.device.test.ts'], { cwd: root, encoding: 'utf8' })
    .split('\n')
    .filter((f) => f && !f.endsWith('.manual.device.test.ts'));
  const pinned = (source: string) => {
    const found = new Set<string>();
    for (const m of source.matchAll(/devices: \['(dt2|dn2)'\]|describeOn\('(dt2|dn2)'/g)) found.add(m[1] ?? m[2]);
    // the SHOT DN2 files run through the helper that pins DN2
    if (/\bdn2Tests\(/.test(source)) found.add('dn2');
    return found;
  };
  const mismatches: string[] = [];
  for (const file of files) {
    let dir = file;
    let project: any;
    for (;;) {
      dir = dir.includes('/') ? dir.slice(0, dir.lastIndexOf('/')) : '';
      try {
        project = JSON.parse(readFileSync(join(root, dir, 'project.json'), 'utf8'));
        break;
      } catch {
        if (!dir) break;
      }
    }
    const rel = file.slice(dir.length + 1);
    const recs: any[] = project?.metadata?.runtimeAcceptance ?? [];
    const declared = recs.find((rec) => rec.files?.includes(rel))?.devices;
    const pins = pinned(readFileSync(join(root, file), 'utf8'));
    // a file whose source pins a device in only some tests, or several devices, is left on both
    const expected =
      pins.size === 1 && !/describeOn\(\s*\w+\./.test(readFileSync(join(root, file), 'utf8')) ? [...pins] : undefined;
    if (JSON.stringify(declared?.length === 2 ? undefined : declared) !== JSON.stringify(expected))
      mismatches.push(file);
  }
  expect(mismatches).toEqual([]);
});
