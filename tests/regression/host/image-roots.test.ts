import fs from 'node:fs';
import path from 'node:path';
import { afterAll, expect, test, vi } from 'vitest';

const dir = vi.hoisted(() => {
  const nodeFs = require('node:fs') as typeof import('node:fs');
  const nodeOs = require('node:os') as typeof import('node:os');
  const nodePath = require('node:path') as typeof import('node:path');
  return nodeFs.mkdtempSync(nodePath.join(nodeOs.tmpdir(), 'image-roots-'));
});
vi.mock('@digi/test-support', async (original) => ({ ...(await original<object>()), repoRoot: () => dir }));
import { projectRootFiles, rootSets, rootsOf } from '../runtime/image-roots.ts';

afterAll(() => fs.rmSync(dir, { recursive: true, force: true }));
const put = (file: string, sets: unknown[]) => {
  fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
  fs.writeFileSync(path.join(dir, file), JSON.stringify({ sets }));
};

test('the lists of a project file are found next to the shared ones; a repeated id is refused', () => {
  put('tests/regression/runtime/image-roots.json', [{ id: 'rt', roots: ['core/runtime'] }]);
  put('modules/core/services/x/tests/image-roots.json', [{ id: 'x-img', roots: ['core/runtime', 'test/x'], devices: ['dn2'] }]);
  expect(projectRootFiles()).toEqual(['modules/core/services/x/tests/image-roots.json']);
  expect(rootSets().map((s) => s.id)).toEqual(['rt', 'x-img']);
  expect(rootsOf('x-img')).toEqual(['core/runtime', 'test/x']);
  expect(() => rootsOf('missing')).toThrow(/no root set missing/);
  put('modules/other/tests/image-roots.json', [{ id: 'rt', roots: [] }]);
  expect(() => rootSets()).toThrow(/root set rt is in both/);
});
