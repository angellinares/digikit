// Runs a command with the pinned Rust toolchain and, with --gen, the generated SHARC core.
// It replaces tools/devtools/rust_ws.py and the build-path part of tools/emulator/digi_env.py,
// so Rust targets need no Python. Usage:
//   node tools/nx/rust-env.mjs [--gen] [--variant NAME] -- CMD...
// The fingerprint rule must match digi_env.core_fingerprint, and the target dir rule must match
// native_target.target_dir: the Python tools still build into the same dirs. targetDir uses an
// explicit gen, where native_target falls back to $SHARC_GEN_DIR.
import { createHash } from 'node:crypto';
import { existsSync, globSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { parse } from 'smol-toml';

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
export const TOOLCHAIN = parse(readFileSync(join(ROOT, 'mise.toml'), 'utf8')).tools.rust;
export const DEFAULT_GENERATED = join(ROOT, 'out', 'native', 'dn2-audio', 'gen');

const CORE_FINGERPRINT_FILE = 'CORE_FINGERPRINT';

const isDir = (path) => {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
};

const real = (path) => {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
};

// SHARC_GEN_DIR (relative to the repo root), else the DN2 prod default.
export const generatedDir = (env = process.env) => {
  const configured = env.SHARC_GEN_DIR;
  if (!configured) return DEFAULT_GENERATED;
  return isAbsolute(configured) ? configured : join(ROOT, configured);
};

// SHA-256 over the sources a generated core is made from, with paths relative to tools/sharc_dev.
export const coreFingerprint = (root = ROOT) => {
  const base = join(root, 'tools', 'sharc_dev');
  const files = [
    ...globSync('sharc_core/**/*.py', { cwd: base }),
    ...globSync('sharc_transpile/**/*.py', { cwd: base }),
    'sharc_transpile_infer.py',
    'sharc_rsgen.py',
  ].map((file) => file.split(sep).join('/'));
  files.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const digest = createHash('sha256');
  for (const file of files) {
    digest.update(Buffer.concat([Buffer.from(file, 'utf8'), Buffer.from([0])]));
    digest.update(Buffer.concat([readFileSync(join(base, file)), Buffer.from([0])]));
  }
  return digest.digest('hex');
};

// Refuses a generated core not made from the current sharc_core (DIGI_ALLOW_STALE_GEN=1 warns instead).
export const checkGenerated = (gen, env = process.env) => {
  if (!isDir(gen)) {
    console.error(`generated core dir is missing: ${gen}`);
    process.exit(1);
  }
  let have;
  try {
    have = readFileSync(join(gen, CORE_FINGERPRINT_FILE), 'utf8').trim();
  } catch {
    have = 'missing';
  }
  const want = coreFingerprint();
  if (have === want) return;
  const message =
    `generated core ${gen} is stale: its ${CORE_FINGERPRINT_FILE} is ${have}, ` +
    `tools/sharc_dev/sharc_core is ${want}.\n` +
    'Regenerate it: DT2: mise run perf:aot -- dt2 --card-image IMG; ' +
    'DN2: uv run python -m tools.sharc_dev.sharc_dn2_aot (recipe in gen/PROVENANCE.txt); ' +
    'shared core: mise run gen:core.\n' +
    'Or set DIGI_ALLOW_STALE_GEN=1 to use it anyway.';
  if (env.DIGI_ALLOW_STALE_GEN === '1') {
    console.error(`warning: ${message.split('\n')[0]} (DIGI_ALLOW_STALE_GEN=1)`);
    return;
  }
  console.error(message);
  process.exit(1);
};

// Cargo target dir for a generated core: out/native/target (main tree) or dist/native-target (worktree), then <key>[/<variant>].
export const targetDir = (gen, variant = '') => {
  const root =
    existsSync(join(ROOT, '.git')) && statSync(join(ROOT, '.git')).isFile()
      ? join(ROOT, 'dist', 'native-target')
      : join(ROOT, 'out', 'native', 'target');
  let key;
  if (!gen || !isDir(gen)) key = 'nogen';
  else {
    const resolved = real(gen);
    if (resolved === real(DEFAULT_GENERATED)) key = 'prod';
    else if (!existsSync(join(resolved, 'image.rs'))) key = 'generic';
    else key = 'g-' + createHash('sha256').update(resolved, 'utf8').digest('hex').slice(0, 8);
  }
  const path = join(root, key);
  return variant ? join(path, variant) : path;
};

// A copy of the environment with RUSTC/RUSTDOC pinned to the toolchain, so PGO profiles keep matching.
export const toolchainEnv = (env = process.env) => {
  const out = { ...env };
  const done = spawnSync('rustup', ['which', '--toolchain', TOOLCHAIN, 'rustc'], { encoding: 'utf8' });
  const rustc = done.status === 0 ? (done.stdout ?? '').trim() : '';
  if (rustc) {
    out.RUSTC = rustc;
    out.RUSTDOC = join(dirname(rustc), 'rustdoc');
    out.PATH = `${dirname(rustc)}:${out.PATH ?? ''}`;
  } else {
    console.error(
      `WARNING: rustup toolchain ${TOOLCHAIN} not found; cargo will use the rustc on PATH (PGO profiles may not match)`,
    );
  }
  return out;
};

const main = (argv) => {
  const split = argv.indexOf('--');
  const opts = split < 0 ? argv : argv.slice(0, split);
  const cmd = split < 0 ? [] : argv.slice(split + 1);
  let withGen = false;
  let variant = '';
  for (let i = 0; i < opts.length; i++) {
    if (opts[i] === '--gen') withGen = true;
    else if (opts[i] === '--variant') variant = opts[++i] ?? '';
    else {
      console.error(`rust-env: unknown option ${opts[i]}`);
      process.exit(2);
    }
  }
  if (!cmd.length) {
    console.error('usage: node tools/nx/rust-env.mjs [--gen] [--variant NAME] -- CMD...');
    process.exit(2);
  }
  const env = toolchainEnv();
  if (withGen) {
    const gen = generatedDir(env);
    checkGenerated(gen, env);
    env.SHARC_GEN_DIR = gen;
    env.CARGO_TARGET_DIR ||= targetDir(gen, variant);
  } else {
    delete env.SHARC_GEN_DIR;
    delete env.SHARC_GEN_DIR_2;
    env.CARGO_TARGET_DIR ||= targetDir('', variant);
  }
  const [head, ...rest] = cmd;
  const run = head === 'cargo' ? ['rustup', ['run', TOOLCHAIN, 'cargo', ...rest]] : [head, rest];
  const done = spawnSync(run[0], run[1], { stdio: 'inherit', env, cwd: ROOT });
  if (done.error) {
    console.error(`rust-env: ${done.error.message}`);
    process.exit(1);
  }
  process.exit(done.status ?? 1);
};

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2));
