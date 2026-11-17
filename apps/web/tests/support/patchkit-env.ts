// What the tests that compare patchkit.wasm (or another wasm) with the patchkit command line share: the repository root, the wasm, the stock images
// and a Python module runner. The tests skip when the wasm, an image or the CLI is absent.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');
export const wasmPath = join(root, 'apps/web/public/patchkit.wasm');
/** The stock images: $DIGI_FIRMWARE_DIR (relative to the repository root), else firmware/ (as tools/firmware/config.py). */
export const firmwareDir = process.env.DIGI_FIRMWARE_DIR
  ? resolve(root, process.env.DIGI_FIRMWARE_DIR)
  : join(root, 'firmware');
export const images: Record<'dt2' | 'dn2', string> = {
  dt2: join(firmwareDir, 'Digitakt_II_OS1.17.syx'),
  dn2: join(firmwareDir, 'Digitone_II_OS1.12.syx'),
};
/** The wasm and both stock images are present. */
export const haveWasmAndImages = [wasmPath, ...Object.values(images)].every(existsSync);

/** Run `python -m <module> args...` from the repository root and return its output. */
export const py = (module: string, ...args: string[]): string =>
  execFileSync('uv', ['run', 'python', '-m', module, ...args], {
    cwd: root,
    env: { ...process.env, PYTHONPATH: root },
    encoding: 'utf8',
    maxBuffer: 1 << 28,
  });

/** A new temporary directory. */
export const workDir = (prefix: string): string => mkdtempSync(join(tmpdir(), prefix));

/** The patchkit module bundle the command line writes (`patchkit bundle`), at `<dir>/bundle.json`. */
export function writeBundle(dir: string): string {
  const path = join(dir, 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', path);
  return path;
}

/** A file as plain bytes (what the wasm loaders take). */
export const readBytes = (path: string): Uint8Array<ArrayBuffer> => new Uint8Array(readFileSync(path));

/** Every module.toml under `dir` (default: modules/). */
export const manifestFiles = (dir = join(root, 'modules')): string[] =>
  readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? manifestFiles(join(dir, e.name)) : e.name === 'module.toml' ? [join(dir, e.name)] : [],
  );
