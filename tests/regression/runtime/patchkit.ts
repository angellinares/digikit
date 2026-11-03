import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { repoRoot } from '@digi/test-support';

const exec = promisify(execFile);
const root = repoRoot();

/** The patchkit binary: `DIGI_PATCHKIT` (the gate sets it), else dist/patchkit/patchkit (nx run patchkit:cli). */
export function patchkitBinary(): string {
  const given = process.env.DIGI_PATCHKIT;
  if (given) {
    if (!fs.existsSync(given)) throw new Error(`DIGI_PATCHKIT names a missing file: ${given}`);
    return given;
  }
  const binary = path.join(root, 'dist/patchkit/patchkit');
  if (!fs.existsSync(binary)) throw new Error(`missing ${binary}: run nx run patchkit:cli`);
  return binary;
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
