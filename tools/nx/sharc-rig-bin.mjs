// Builds the SHARC-only rig (packages/sharc-rig) against the shared generated core and installs it as dist/bin/sharc-rig with
// dist/bytes/sharc-rig.sha256 (the hash of the binary, which holds the generated core: the cache key of the tasks that run it).
// The core is out/native/shared-core/gen (`mise run gen:core`), or SHARC_GEN_DIR. CARGO_TARGET_DIR overrides the target dir of
// tools/nx/rust-env.mjs (a lane that built the rig earlier names that dir and skips a cold build).
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { isAbsolute, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { ROOT, generatedDir, targetDir } from './rust-env.mjs';

const env = { ...process.env };
env.SHARC_GEN_DIR ||= join(ROOT, 'out', 'native', 'shared-core', 'gen');
const gen = generatedDir(env);
const target = env.CARGO_TARGET_DIR
  ? isAbsolute(env.CARGO_TARGET_DIR)
    ? env.CARGO_TARGET_DIR
    : join(ROOT, env.CARGO_TARGET_DIR)
  : targetDir(gen);
env.CARGO_TARGET_DIR = target;

const built = spawnSync(
  'node',
  [
    join(ROOT, 'tools/nx/rust-env.mjs'),
    '--gen',
    '--',
    'cargo',
    'build',
    '--release',
    '-p',
    'sharc-rig',
    '--locked',
    '--offline',
  ],
  { stdio: 'inherit', env, cwd: ROOT },
);
if (built.status !== 0) process.exit(built.status ?? 1);

// copy, then rename: a running process keeps the old file
const bin = join(ROOT, 'dist', 'bin');
mkdirSync(bin, { recursive: true });
mkdirSync(join(ROOT, 'dist', 'bytes'), { recursive: true });
copyFileSync(join(target, 'release', 'sharc-rig'), join(bin, 'sharc-rig.tmp'));
renameSync(join(bin, 'sharc-rig.tmp'), join(bin, 'sharc-rig'));
const hash = createHash('sha256')
  .update(readFileSync(join(bin, 'sharc-rig')))
  .digest('hex');
writeFileSync(join(ROOT, 'dist', 'bytes', 'sharc-rig.sha256'), `${hash}\n`);
console.log(`dist/bin/sharc-rig ${hash}`);
