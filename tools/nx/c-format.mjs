// Formats or checks the hand-written C with the clang-format pinned in mise.toml.
// Usage: node tools/nx/c-format.mjs [--write]
// The files are the tracked and untracked *.c and *.h; .clang-format-ignore lists the generated ones.
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const write = process.argv.includes('--write');
const run = (cmd, args, options = {}) =>
  spawnSync(cmd, args, { cwd: root, encoding: 'utf8', maxBuffer: 1 << 26, ...options });

const listed = run('git', ['ls-files', '-co', '--exclude-standard', '--', '*.c', '*.h']);
if (listed.status !== 0) {
  process.stderr.write(listed.stderr);
  process.exit(listed.status ?? 1);
}
const files = [...new Set(listed.stdout.split('\n').filter(Boolean))].filter((f) => existsSync(resolve(root, f)));
const flags = write ? ['-i'] : ['--dry-run', '--Werror'];
const direct = run('clang-format', ['--version']);
const [cmd, prefix] = direct.error ? ['mise', ['exec', '--', 'clang-format']] : ['clang-format', []];
const result = run(cmd, [...prefix, ...flags, ...files], { stdio: 'inherit' });
if (result.error) {
  process.stderr.write(`cannot run clang-format (pinned in mise.toml): ${result.error.message}\n`);
  process.exit(1);
}
if (result.status !== 0 && !write) process.stderr.write('C format differs: run `mise run fmt:c`.\n');
process.exit(result.status ?? 1);
