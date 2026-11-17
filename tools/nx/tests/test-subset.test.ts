import { expect, it } from 'vitest';
import { invocations, resolveSubset, selectionPairs } from '../test-subset.mjs';

const project = (name: string, targets: string[], runtimeAcceptance: object[]) => ({
  name,
  targets,
  metadata: { runtimeAcceptance },
});
const projects = [
  // two files: per-file leaves
  project(
    'core/a',
    ['test-device', 'test-device--x', 'test-device--x--dt2', 'test-device--x--dn2', 'test-device--y--dn2'],
    [
      { files: ['tests/x.device.test.ts'], tags: ['S:machines', 'C:loading'] },
      { files: ['tests/y.device.test.ts'], tags: ['C:boot'] },
    ],
  ),
  // one file: the leaves have no file part
  project(
    'core/b',
    ['test-device', 'test-device--dt2', 'test-device--dn2'],
    [{ files: ['b.device.test.ts'], tags: ['S:machines'] }],
  ),
  project('gate-smoke', ['smoke'], []),
  project('gate-pins', ['pins'], []),
];

it('resolves tags to the leaf targets of both devices, as a union', () => {
  const { tests, unknown } = resolveSubset(['S:machines', 'C:boot'], projects);
  expect(unknown).toEqual([]);
  expect(tests.map((t) => [t.project, t.targets])).toEqual([
    ['core/a', ['test-device--x--dt2', 'test-device--x--dn2']],
    ['core/a', ['test-device--y--dn2']],
    ['core/b', ['test-device--dt2', 'test-device--dn2']],
  ]);
});

it('reports a tag that no record has', () => {
  expect(resolveSubset(['S:fx', 'C:boot'], projects).unknown).toEqual(['S:fx']);
});

it('adds gate-smoke and gate-pins, and runs one call when the cross product is exact', () => {
  const { tests } = resolveSubset(['C:boot'], projects);
  const pairs = selectionPairs(tests, projects);
  expect(pairs.map((p) => `${p.project}:${p.target}`)).toEqual([
    'core/a:test-device--y--dn2',
    'gate-smoke:smoke',
    'gate-pins:pins',
  ]);
  expect(invocations(pairs, projects)).toEqual([
    ['run-many', '-t', 'test-device--y--dn2,smoke,pins', '-p', 'core/a,gate-smoke,gate-pins'],
  ]);
});

it('splits the calls when a project would run a target that was not selected', () => {
  const both = [
    ...projects,
    project('core/c', ['test-device--dn2'], [{ files: ['c.device.test.ts'], tags: ['C:state'] }]),
  ];
  const pairs = [
    { project: 'core/b', target: 'test-device--dt2' },
    { project: 'core/c', target: 'test-device--dn2' },
    { project: 'core/b', target: 'test-device--dn2' },
  ];
  expect(invocations(pairs.slice(0, 2), both)).toEqual([
    ['run-many', '-t', 'test-device--dt2', '-p', 'core/b'],
    ['run-many', '-t', 'test-device--dn2', '-p', 'core/c'],
  ]);
});
