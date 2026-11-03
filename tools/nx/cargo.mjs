// Nx plugin: one project per Cargo workspace member (read from Cargo.toml files, cargo is not run).
// Edges: path dependencies between members, in every dependency table.
// rust-tests is cached for HERMETIC crates (direct cargo nextest into dist/cargo-tests); the others run
// through tools/devtools/rust_ws.py uncached.
import { existsSync, readFileSync } from 'node:fs';
import { join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { parse } from 'smol-toml';

const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));
const KINDS = ['dependencies', 'dev-dependencies', 'build-dependencies'];

// CLI builds into dist/ (gitignored). Copy then rename: a running process keeps the old file.
const CLI = {
  patchkit: {
    build: 'cargo build -p patchkit --release --offline --target-dir dist/cargo',
    bin: 'patchkit',
    dir: 'patchkit',
  },
  'plusdrive-format': {
    build: 'cargo build -p plusdrive-format --bin plusdrive --release --locked --offline --target-dir dist/cargo',
    bin: 'plusdrive',
    dir: 'plusdrive',
  },
};
const cliTarget = ({ build, bin, dir }) => ({
  executor: 'nx:run-commands',
  cache: false,
  outputs: [`{workspaceRoot}/dist/${dir}/${bin}`],
  options: {
    commands: [
      build,
      `mkdir -p dist/${dir} && cp dist/cargo/release/${bin} dist/${dir}/${bin}.tmp && mv -f dist/${dir}/${bin}.tmp dist/${dir}/${bin}`,
    ],
    parallel: false,
    cwd: '.',
  },
});

// Gate helper targets. They build under dist/ or out/native; out/ is never an Nx output.
const GATE = {
  'digi-control': {
    daemons: {
      executor: 'nx:run-commands',
      cache: false,
      outputs: ['{workspaceRoot}/dist/daemons.json'],
      inputs: [
        '{workspaceRoot}/tools/devtools/emud_key.py',
        '{workspaceRoot}/tools/devtools/verify_build.py',
        '{workspaceRoot}/tools/devtools/stablebin.py',
        '{workspaceRoot}/tools/emulator/digi_env.py',
        '{workspaceRoot}/tools/emulator/native_pgo.py',
        '{workspaceRoot}/tools/emulator/native_target.py',
        '{workspaceRoot}/tools/emulator/native_emu.py',
        '{workspaceRoot}/tools/patchkit/pk_emud_pgo.py',
      ],
      options: {
        commands: ['mkdir -p dist', 'uv run python -m tools.devtools.verify_build daemons --env-out dist/daemons.json'],
        parallel: false,
        cwd: '.',
      },
    },
  },
  'digi-coupled': {
    build: {
      executor: 'nx:run-commands',
      cache: false,
      options: { command: 'uv run python -m tools.emulator.coupled build', cwd: '.' },
    },
    'build-dt2': {
      executor: 'nx:run-commands',
      cache: false,
      options: { command: 'uv run python -m tools.emulator.coupled build --device dt2', cwd: '.' },
    },
  },
};

// Crates without tests (rust_ws also drops digiemu-desktop); no rust-tests target.
const NO_TESTS = new Set([
  'sharc-stats',
  'sharc-gen',
  'sharc-gen-2',
  'sharc-native-2',
  'dkfw',
  'sharc-fast-cl',
  'sharc-fast-cl-2',
  'bootstrap-emu',
  'digiemu-desktop',
]);

// Crates whose tests read no firmware, generated core or out/ data; their rust-tests are cached.
// Extra inputs are files compiled in (include_str!). Check with `node tools/nx/cargo-depfiles.mjs` after a run.
export const HERMETIC = {
  coldfire: [],
  periph: [],
  'emmc-card': [],
  dkneeds: [],
  'dkrt-sdk': ['{workspaceRoot}/sdk/vectors/settings-vectors.json'],
  'dkrt-fit': ['{workspaceRoot}/sdk/vectors/fit-vectors.json'],
  'device-profile': ['{workspaceRoot}/devices/digitakt-ii.toml', '{workspaceRoot}/devices/digitone-ii.toml'],
  dkupload: [],
  dksample: [],
};

// Crates that compile another crate's sources by path (sharc-2 builds packages/emulator/sharc/src);
// those files are outside the project root.
const SHARED_SOURCE = { 'sharc-native-2': ['{workspaceRoot}/packages/emulator/sharc/**/*'] };

// Files outside the crate embedded by patchkit's source, unit tests and build.rs.
const COMPILE_SOURCE = {
  patchkit: [
    '{workspaceRoot}/sdk/sdk.toml',
    '{workspaceRoot}/sdk/vectors/fit-vectors.json',
    '{workspaceRoot}/sdk/vectors/settings-vectors.json',
    '{workspaceRoot}/devices/*.toml',
    '{workspaceRoot}/tools/patchkit/toolchain/pins.toml',
    '{workspaceRoot}/tools/patchkit/toolchain/m68k-cf-none-elf-a2.json',
    '{workspaceRoot}/tools/patchkit/pk_toolchains.py',
  ],
};

// Normalized repo-relative roots of the path dependencies of a crate (all dependency tables).
export const pathDeps = (workspaceRoot, crateRoot) => {
  const t = read(workspaceRoot, join(crateRoot, 'Cargo.toml'));
  const tables = [t, ...Object.values(t.target ?? {})].flatMap((s) => KINDS.map((k) => s[k] ?? {}));
  const deps = tables.flatMap((tbl) =>
    Object.values(tbl)
      .filter((d) => d?.path)
      .map((d) => normalize(join(crateRoot, d.path))),
  );
  return [...new Set(deps)];
};

export const createNodesV2 = [
  'Cargo.toml',
  (files, _opts, ctx) =>
    files.map((f) => {
      const inputs = ['default', '^production', '{workspaceRoot}/Cargo.toml', '{workspaceRoot}/Cargo.lock'];
      if (existsSync(join(ctx.workspaceRoot, 'rust-toolchain.toml')))
        inputs.push('{workspaceRoot}/rust-toolchain.toml');
      const rust = read(ctx.workspaceRoot, 'mise.toml').tools.rust;
      const hermeticInputs = [
        'default',
        '^production',
        '{workspaceRoot}/Cargo.toml',
        '{workspaceRoot}/Cargo.lock',
        ...(existsSync(join(ctx.workspaceRoot, 'rust-toolchain.toml')) ? ['{workspaceRoot}/rust-toolchain.toml'] : []),
        { runtime: `rustup run ${rust} rustc -vV` },
        { env: 'RUSTFLAGS' },
        { env: 'CARGO_ENCODED_RUSTFLAGS' },
        { env: 'CARGO_BUILD_TARGET' },
        { env: 'RUSTC_WRAPPER' },
        { env: 'RUSTC_WORKSPACE_WRAPPER' },
        { env: 'RUST_TEST_THREADS' },
      ];
      const rustTests = (name) =>
        name in HERMETIC
          ? {
              executor: 'nx:run-commands',
              cache: true,
              inputs: [...hermeticInputs, ...HERMETIC[name]],
              outputs: [],
              options: {
                command: `rustup run ${rust} cargo nextest run -p ${name} --release --locked --offline --target-dir dist/cargo-tests`,
                cwd: '.',
              },
            }
          : {
              executor: 'nx:run-commands',
              cache: false,
              inputs: [...inputs, ...(SHARED_SOURCE[name] ?? []), ...(COMPILE_SOURCE[name] ?? [])],
              options: { command: `uv run python -m tools.devtools.rust_ws test --packages ${name}`, cwd: '.' },
            };
      const projects = {};
      for (const root of read(ctx.workspaceRoot, f).workspace?.members ?? []) {
        const { name } = read(ctx.workspaceRoot, join(root, 'Cargo.toml')).package;
        projects[root] = {
          name,
          root,
          tags: ['rust'],
          targets: {
            ...(!NO_TESTS.has(name) && { 'rust-tests': rustTests(name) }),
            ...(CLI[name] && { cli: cliTarget(CLI[name]) }),
            ...GATE[name],
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
    return pathDeps(ctx.workspaceRoot, p.root)
      .map((path) => mods.find((m) => m.root === path)?.name)
      .filter((target) => target && target !== p.name)
      .map((target) => ({ source: p.name, target, sourceFile: file, type: DependencyType.static }));
  });
};
