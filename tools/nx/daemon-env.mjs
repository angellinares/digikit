// Runs a command with DIGI_EMUD_<DEV> from dist/daemons.json (written by digi-control:daemons), so the Python
// harnesses (smoke, card_check) use the same content-keyed daemon as the Vitest device tests.
// Variables already set in the environment win.
// Usage: node tools/nx/daemon-env.mjs -- CMD [ARGS...]
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const sep = process.argv.indexOf('--');
const argv = sep < 0 ? [] : process.argv.slice(sep + 1);
if (!argv.length) {
  console.error('usage: node tools/nx/daemon-env.mjs -- CMD [ARGS...]');
  process.exit(2);
}
const file = resolve(root, 'dist/daemons.json');
if (!existsSync(file)) {
  console.error('daemon-env: dist/daemons.json missing: run pnpm nx:pilot run digi-control:daemons');
  process.exit(2);
}
const env = { ...JSON.parse(readFileSync(file, 'utf8')), ...process.env };
const result = spawnSync(argv[0], argv.slice(1), { stdio: 'inherit', env });
if (result.error) {
  console.error(result.error);
  process.exit(1);
}
process.exit(result.status ?? 1);
