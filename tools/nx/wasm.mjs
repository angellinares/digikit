#!/usr/bin/env node
// Build the portable native runtime for the browser without changing toolchains.
//
// Writes five files:
//   apps/web/public/patchkit.wasm
//       The patch tool of packages/patchkit (C ABI, no firmware inside: ports and modules are passed in at run time).
//   apps/web/public/dkupload.wasm
//       The module upload protocol of packages/dkupload (C ABI, no firmware inside).
//   apps/web/public/dksample.wasm
//       The WAV to native sample converter of packages/dksample (plusdrive-format's, C ABI, no firmware inside).
//   apps/web/public/emulator-core.wasm
//       CF-only core (small), used for the normal boot.
//   apps/web/.local/emulator-core-sharc.wasm
//       coupled ColdFire + SHARC+ core (large, firmware-derived, gitignored). The
//       dev server serves it; it must never be in public/ or dist/. Built when a
//       generated core is available and --cf-only is not given; --cf-only leaves an
//       existing copy in place (it may then be older than emulator-core.wasm);
//       without a generated core the copy is removed. A stale copy in
//       apps/web/public is deleted on every run.

import { spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { parseArgs } from 'node:util';
import { DEFAULT_GENERATED, ROOT, checkGenerated, targetDir, toolchainEnv } from './rust-env.mjs';

const USAGE = `usage: wasm.mjs [options]

  --diagnostics     include bounded PC profiling and event history
  --cf-only         build only the CF-only browser core, even when a generated core is available
  --sharc           fail unless the coupled SHARC+ core (emulator-core-sharc.wasm) can be built
                    (needs SHARC_GEN_DIR or the default generated core)
  --patchkit-only   build only public/patchkit.wasm
  --dksample-only   build only public/dksample.wasm (\`mise run workbench\` runs this first)
  --no-fast         leave the fast tier (feature fast-wasm, off until the page turns it on) out of the coupled core
  --out PATH        output path of the coupled SHARC+ core (default apps/web/.local/emulator-core-sharc.wasm)
  --wasm-opt STR    run wasm-opt with these flags on the coupled SHARC+ core only (default -O3;
                    --wasm-opt= turns it off); the raw build stays next to the output as <stem>.raw.wasm
`;

function die(message) {
  process.stderr.write(`${message}\n`);
  process.exit(1);
}

function which(name) {
  for (const dir of (process.env.PATH ?? '').split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, name);
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      if (fs.statSync(candidate).isFile()) return candidate;
    } catch {
      // not here
    }
  }
  return null;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  return result.status === 0 && !result.error;
}

let args;
try {
  ({ values: args } = parseArgs({
    options: {
      diagnostics: { type: 'boolean', default: false },
      'cf-only': { type: 'boolean', default: false },
      sharc: { type: 'boolean', default: false },
      'patchkit-only': { type: 'boolean', default: false },
      'dksample-only': { type: 'boolean', default: false },
      'no-fast': { type: 'boolean', default: false },
      out: { type: 'string' },
      'wasm-opt': { type: 'string', default: '-O3' },
      help: { type: 'boolean', short: 'h', default: false },
    },
    strict: true,
  }));
} catch (error) {
  process.stderr.write(`${USAGE}\n${error.message}\n`);
  process.exit(2);
}
if (args.help) {
  process.stdout.write(USAGE);
  process.exit(0);
}

const configuredGenerated = process.env.SHARC_GEN_DIR;
let generated = configuredGenerated || String(DEFAULT_GENERATED);
const isDir = (p) => {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
};
if (generated && !isDir(generated)) {
  if (configuredGenerated || args.sharc) {
    die('SHARC_GEN_DIR must name a local generated DN2 core directory');
  }
  generated = null;
}
if (args.sharc && args['cf-only']) {
  die('--sharc and --cf-only cannot be used together');
}
const coupled = Boolean(generated) && !args['cf-only'];
if (coupled) {
  checkGenerated(generated, process.env);
}

function resolveTargetDir(gen) {
  // Shared target dir keyed by the generated core (see rust-env.mjs).
  // CARGO_TARGET_DIR still wins for both builds; with it set the two builds
  // share one dir, which works but rebuilds the crate each time.
  // gen is '' for builds without a generated core.
  const dir = process.env.CARGO_TARGET_DIR || targetDir(gen);
  return path.isAbsolute(dir) ? dir : path.join(ROOT, dir);
}

const baseEnv = { ...toolchainEnv(process.env) };
let rustc = baseEnv.RUSTC;
if (!rustc) {
  rustc = which('rustc');
  if (rustc) {
    baseEnv.PATH = `${path.dirname(rustc)}:${process.env.PATH}`;
    baseEnv.RUSTC = rustc;
    baseEnv.RUSTDOC = path.join(path.dirname(rustc), 'rustdoc');
  }
}
if (!rustc) {
  die('rustc is required; install the pinned mise Rust toolchain first');
}
const sysrootResult = spawnSync(rustc, ['--print', 'sysroot'], { encoding: 'utf8' });
if (sysrootResult.status !== 0) {
  die(`rustc --print sysroot failed: ${sysrootResult.stderr ?? sysrootResult.error}`);
}
baseEnv.DYLD_FALLBACK_LIBRARY_PATH = `${sysrootResult.stdout.trim()}/lib`;

const publicDir = path.join(ROOT, 'apps/web/public');
fs.mkdirSync(publicDir, { recursive: true });
const localDir = path.join(ROOT, 'apps/web/.local');
fs.mkdirSync(localDir, { recursive: true });
const stale = path.join(publicDir, 'emulator-core-sharc.wasm');
if (fs.existsSync(stale)) {
  fs.unlinkSync(stale);
  console.log(`removed stale ${stale} (the coupled core now lives in apps/web/.local)`);
}

function withoutGenDir(env) {
  const copy = { ...env };
  delete copy.SHARC_GEN_DIR;
  return copy;
}

function build(features, targetDirPath, env, dest) {
  const command = [
    'build',
    '--manifest-path',
    'packages/emulator/boot/Cargo.toml',
    '--locked',
    '--offline',
    '--release',
    '--lib',
    '--target',
    'wasm32-unknown-unknown',
    '--target-dir',
    targetDirPath,
  ];
  if (features.length) command.push('--features', features.join(','));
  if (!run('cargo', command, { cwd: ROOT, env })) {
    die('WASM build failed; install wasm32-unknown-unknown for the pinned Rust toolchain');
  }
  fs.copyFileSync(path.join(targetDirPath, 'wasm32-unknown-unknown/release/elektron_native_boot.wasm'), dest);
}

const WASM_FEATURE_FLAGS = [
  '--enable-bulk-memory',
  '--enable-sign-ext',
  '--enable-mutable-globals',
  '--enable-nontrapping-float-to-int',
  '--enable-multivalue',
  '--enable-reference-types',
];

// Small wasm32 cdylibs of packages/<name> (no generated core, no firmware), copied to public/<name>.wasm.
// One cargo call builds all of them: separate calls (one per manifest) give each its own feature set for the crates they share, so each
// call made the next one rebuild them (about 10 s on every run with nothing changed).
function buildSmalls(names) {
  const target = resolveTargetDir('');
  const command = [
    'build',
    '--locked',
    '--offline',
    '--release',
    '--lib',
    ...names.flatMap((name) => ['-p', name]),
    '--target',
    'wasm32-unknown-unknown',
    '--target-dir',
    target,
  ];
  if (!run('cargo', command, { cwd: ROOT, env: withoutGenDir(baseEnv) })) {
    die(`${names.join(', ')} WASM build failed`);
  }
  for (const name of names) {
    const built = path.join(target, `wasm32-unknown-unknown/release/${name}.wasm`);
    const dest = path.join(publicDir, `${name}.wasm`);
    // wasm-opt is slow and its result depends only on the built file (and the flags): keep the hash it ran for.
    const stamp = path.join(localDir, `${name}.wasm.opt-stamp`);
    const builtHash = crypto
      .createHash('sha256')
      .update(fs.readFileSync(built))
      .update(WASM_FEATURE_FLAGS.join(' '))
      .digest('hex');
    const haveOpt = which('wasm-opt') !== null;
    const sha = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    // the stamp also names the output it made: apps/web/.local is shared by worktrees, public/ is not
    const stampFor = (file) => `${builtHash} ${haveOpt} ${sha(file)}\n`;
    const current = fs.existsSync(dest) && fs.existsSync(stamp) && fs.readFileSync(stamp, 'utf8') === stampFor(dest);
    if (!current) {
      const tmp = path.join(publicDir, `${name}.wasm.${process.pid}.tmp`);
      if (haveOpt) {
        if (!run('wasm-opt', ['-Oz', ...WASM_FEATURE_FLAGS, built, '-o', tmp])) {
          die(`wasm-opt failed on ${name}.wasm`);
        }
      } else {
        fs.copyFileSync(built, tmp);
      }
      // write only when the bytes differ, so a reader (the app) and file watchers see no change on a run with nothing new
      if (fs.existsSync(dest) && fs.readFileSync(dest).equals(fs.readFileSync(tmp))) {
        fs.rmSync(tmp);
      } else {
        fs.renameSync(tmp, dest);
      }
      fs.writeFileSync(stamp, stampFor(dest));
    }
    // patchkit.wasm carries the Selache assembler (the SHARC+ build, 0.4 MB); the others stay under 2 MiB
    const limit = (name === 'patchkit' ? 5 : 4) * 512 * 1024;
    const size = fs.statSync(dest).size;
    if (size > limit) {
      die(`${dest} is ${size} bytes: ${name}.wasm must stay small and firmware-free`);
    }
    console.log(`${name}.wasm: ${size} bytes${current ? ' (unchanged)' : ''}`);
  }
}

if (args['dksample-only']) {
  buildSmalls(['dksample']);
  process.exit(0);
}
buildSmalls(['patchkit', 'dkupload', 'dksample']);
if (args['patchkit-only']) {
  process.exit(0);
}

// The plain core has no fast tier. The coupled core has one (feature fast-wasm,
// off until the page calls digi_sharc_fast or digi_sharc_fast_regions): it
// reads no SHARC_FAST_REGIONS (wasm32-unknown-unknown has no environment) and
// its kernels are wasm modules the page compiles (apps/web/src/core-host.ts).
const diagnostics = args.diagnostics ? ['diagnostic-profile', 'diagnostic-events'] : [];
build(diagnostics, resolveTargetDir(''), withoutGenDir(baseEnv), path.join(publicDir, 'emulator-core.wasm'));
const sharcWasm = args.out ? path.resolve(args.out) : path.join(localDir, 'emulator-core-sharc.wasm');
if (coupled) {
  const wasmOpt = args['wasm-opt'];
  if (wasmOpt && which('wasm-opt') === null) {
    die('wasm-opt not found: run `mise install` (mise.toml pins binaryen)');
  }
  const ext = path.extname(sharcWasm);
  const stem = path.basename(sharcWasm, ext);
  const rawWasm = path.join(path.dirname(sharcWasm), `${stem}.raw.wasm`);
  build(
    [...diagnostics, 'sharc', ...(args['no-fast'] ? [] : ['fast-wasm'])],
    resolveTargetDir(generated),
    { ...baseEnv, SHARC_GEN_DIR: generated },
    wasmOpt ? rawWasm : sharcWasm,
  );
  if (wasmOpt) {
    const started = process.hrtime.bigint();
    // Python used shlex.split; splitting on whitespace is enough for these flags.
    const flags = wasmOpt.split(/\s+/).filter(Boolean);
    if (!run('wasm-opt', [...flags, ...WASM_FEATURE_FLAGS, rawWasm, '-o', sharcWasm])) {
      die('wasm-opt failed on the coupled SHARC+ core');
    }
    const seconds = Number(process.hrtime.bigint() - started) / 1e9;
    console.log(
      `wasm-opt: raw ${fs.statSync(rawWasm).size} bytes, optimised ${fs.statSync(sharcWasm).size} bytes, ${seconds.toFixed(1)} s`,
    );
  }
} else if (args['cf-only']) {
  const rel = path.relative(ROOT, sharcWasm);
  const shown = rel && !rel.startsWith('..') && !path.isAbsolute(rel) ? rel : sharcWasm;
  console.log(`coupled SHARC+ core skipped (--cf-only); kept ${shown} as it was`);
} else {
  fs.rmSync(sharcWasm, { force: true });
  console.log('coupled SHARC+ core skipped (no generated core); removed any stale emulator-core-sharc.wasm');
}
