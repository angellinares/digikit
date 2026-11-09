// The fit rule of the tests: the Rust rule in apps/web/public/patchkit.wasm (`mise run wasm:patchkit`). A suite that checks code which asks the
// rule (mergeView, tickedFit) calls `await useFitWasm()` once at the top.
import { existsSync, readFileSync } from 'node:fs';
import { DkNeeds } from '../src/modules/dkneeds.ts';
import { setFitReader } from '../src/modules/fit.ts';

export const WASM = new URL('../public/patchkit.wasm', import.meta.url);

export async function useFitWasm(): Promise<void> {
  if (!existsSync(WASM)) throw new Error('apps/web/public/patchkit.wasm is not built: run `mise run wasm:patchkit`');
  setFitReader(await DkNeeds.create(new Uint8Array(readFileSync(WASM))));
}
