import fs from 'node:fs';

/**
 * Temp dirs of the fixtures of this test file (artifacts). A test may read the artifacts of an earlier fixture of its file, so they are
 * removed when the file ends (`flushFileTemps`, run by `setup-temp.ts` in a root `afterAll`), not when a fixture closes. They stay, with a
 * line that names them, when a fixture of the file failed.
 */
const state = { dirs: new Set<string>(), failed: false };

export function fileTemp(dir: string, failed: boolean): void {
  state.dirs.add(dir);
  state.failed ||= failed;
}

/** Remove the dirs of the file, or keep them (and print them) when a fixture failed; empties the registry. */
export function flushFileTemps(): void {
  for (const dir of state.dirs) {
    if (state.failed) console.info(`kept run dir for diagnostics: ${dir}`);
    else fs.rmSync(dir, { recursive: true, force: true });
  }
  state.dirs.clear();
  state.failed = false;
}
