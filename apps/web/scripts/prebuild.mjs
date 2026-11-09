// Build preparation, run from apps/web: the wasm artifacts, then the module data.
import { execFileSync } from 'node:child_process';
import { SHOW_EMULATOR } from '../site.config.mjs';

const run = (args) => execFileSync(process.execPath, args, { stdio: 'inherit' });

run(['../../tools/nx/wasm.mjs', ...(SHOW_EMULATOR ? [] : ['--patchkit-only'])]);
run(['scripts/gen-modules.mjs']);
