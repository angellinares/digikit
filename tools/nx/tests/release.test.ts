import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { stripVTControlCharacters } from 'node:util';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, expect, it } from 'vitest';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const scratch = mkdtempSync(join(tmpdir(), 'digikit-release-dry-run-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));
const git = (...args: string[]) => execFileSync('git', args, { cwd: root });
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
const env = {
  ...process.env,
  NX_DAEMON: 'false',
  NX_NO_CLOUD: 'true',
  NX_ISOLATE_PLUGINS: 'false',
  NX_WORKSPACE_DATA_DIRECTORY: join(scratch, 'workspace-data'),
};
let moduleProject: string;
let moduleManifest: string;
beforeAll(() => {
  const file = join(scratch, 'graph.json');
  execFileSync('pnpm', ['exec', 'nx', 'graph', `--file=${file}`], { cwd: root, env, stdio: 'pipe', timeout: 30_000 });
  const graph = JSON.parse(readFileSync(file, 'utf8')).graph;
  const [id, node] = Object.entries(graph.nodes)
    .filter(([, node]) => (node as { data: { tags?: string[] } }).data.tags?.includes('release-module'))
    .sort(([a], [b]) => a.localeCompare(b))[0] as [string, { data: { root: string } }];
  moduleProject = id;
  moduleManifest = `${node.data.root}/module.toml`;
});

function sourceState() {
  const untracked = git('ls-files', '--others', '--exclude-standard', '-z').toString().split('\0').filter(Boolean);
  return {
    diff: hash(git('diff', '--binary', 'HEAD')),
    untracked: Object.fromEntries(untracked.map((path) => [path, hash(readFileSync(join(root, path)))])),
    tags: git('for-each-ref', '--format=%(refname) %(objectname)', 'refs/tags').toString(),
    index: hash(readFileSync(resolve(root, git('rev-parse', '--git-path', 'index').toString().trim()))),
  };
}

it.each(['module', 'runtime'])(
  'dry-runs a %s release in isolation without source, index or tag changes',
  (kind) => {
    const project = kind === 'module' ? moduleProject : 'core/runtime';
    const expected = kind === 'module' ? [moduleManifest] : ['sdk/sdk.toml', 'modules/core/runtime/module.toml'];
    const before = sourceState();
    const output = execFileSync(
      'pnpm',
      ['exec', 'nx', 'release', 'version', 'patch', `--projects=${project}`, '--dry-run'],
      {
        cwd: root,
        env,
        timeout: 30_000,
        encoding: 'utf8',
        stdio: 'pipe',
      },
    );
    // The adapter's own update messages are available even when Nx suppresses UPDATE headings in CI.
    const proposed = [
      ...stripVTControlCharacters(output).matchAll(/ (\S+\.toml): \d+\.\d+\.\d+ -> \d+\.\d+\.\d+$/gm),
    ].map((match) => match[1]);
    expect(sourceState()).toEqual(before);
    expect(proposed.sort(), stripVTControlCharacters(output)).toEqual([...expected].sort());
  },
  30_000,
);
