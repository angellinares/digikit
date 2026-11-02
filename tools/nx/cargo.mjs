// Nx plugin: one project per Cargo workspace member (read from Cargo.toml files, cargo is not run).
// Edges: path dependencies between members, in every dependency table.
import { existsSync, readFileSync } from 'node:fs';
import { join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { parse } from 'smol-toml';

const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));
const KINDS = ['dependencies', 'dev-dependencies', 'build-dependencies'];

export const createNodesV2 = [
  'Cargo.toml',
  (files, _opts, ctx) =>
    files.map((f) => {
      const inputs = ['default', '{workspaceRoot}/Cargo.toml', '{workspaceRoot}/Cargo.lock'];
      if (existsSync(join(ctx.workspaceRoot, 'rust-toolchain.toml')))
        inputs.push('{workspaceRoot}/rust-toolchain.toml');
      const projects = {};
      for (const root of read(ctx.workspaceRoot, f).workspace?.members ?? []) {
        const { name } = read(ctx.workspaceRoot, join(root, 'Cargo.toml')).package;
        projects[root] = {
          name,
          root,
          tags: ['rust'],
          targets: {
            'rust-tests': {
              executor: 'nx:run-commands',
              cache: false,
              inputs,
              options: { command: `uv run python -m tools.devtools.rust_ws test --packages ${name}`, cwd: '.' },
            },
          },
        };
      }
      return [f, { projects }];
    }),
];

export const createDependencies = (_opts, ctx) => {
  const mods = Object.values(ctx.projects).filter((p) => p.tags?.includes('rust'));
  return mods.flatMap((p) => {
    const file = join(p.root, 'Cargo.toml');
    const t = read(ctx.workspaceRoot, file);
    const tables = [t, ...Object.values(t.target ?? {})].flatMap((s) => KINDS.map((k) => s[k] ?? {}));
    const deps = tables.flatMap((tbl) =>
      Object.values(tbl)
        .filter((d) => d?.path)
        .map((d) => normalize(join(p.root, d.path))),
    );
    return [...new Set(deps)]
      .map((path) => mods.find((m) => m.root === path)?.name)
      .filter((target) => target && target !== p.name)
      .map((target) => ({ source: p.name, target, sourceFile: file, type: DependencyType.static }));
  });
};
