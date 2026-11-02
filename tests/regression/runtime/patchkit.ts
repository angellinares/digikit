import { execFile, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { promisify } from 'node:util';
import { repoRoot } from '@digi/test-support';

const exec = promisify(execFile);
const root = repoRoot();
let built: string | undefined;

/** The patchkit binary: `DIGI_PATCHKIT` (the gate sets it), else the stable copy `patchkit.py bin` builds once per process. */
export function patchkitBinary(): string {
  const given = process.env.DIGI_PATCHKIT;
  if (given) {
    if (!fs.existsSync(given)) throw new Error(`DIGI_PATCHKIT names a missing file: ${given}`);
    return given;
  }
  built ??= execFileSync('uv', ['run', 'python', '-m', 'tools.patchkit.patchkit', 'bin'], {
    cwd: root,
    encoding: 'utf8',
    env: { ...process.env, UV_NO_SYNC: '1' },
  }).trim();
  return built;
}

/** Runs the binary on the repository root (`DIGI_PK_ROOT` names a module variant). `input` goes to stdin. */
export function patchkit(args: string[], { input, timeout = 30_000 }: { input?: string; timeout?: number } = {}) {
  const run = exec(patchkitBinary(), ['--root', process.env.DIGI_PK_ROOT || root, ...args], {
    cwd: root,
    timeout,
    maxBuffer: 4 * 1024 * 1024,
    encoding: 'utf8',
  });
  if (input !== undefined) run.child.stdin?.end(input);
  return run;
}
