/** The Vitest entry of a module's sound checks: `rigSuite(import.meta.dirname)` in modules/<kind>/<name>/tests/rig.rig.test.ts runs every `rig/*.toml` there. */
import { readdirSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { loadRigFile, runRigFile } from './soundcheck.ts';
import { moduleIdOfDir } from './spec.ts';

export function rigSuite(testsDir: string): void {
  const module = moduleIdOfDir(path.dirname(testsDir));
  const dir = path.join(testsDir, 'rig');
  for (const name of readdirSync(dir)
    .filter((f) => f.endsWith('.toml'))
    .sort()) {
    const file = path.join(dir, name);
    describe(`${module} ${name}`, () => {
      const cases = loadRigFile(file).case.map((c) => c.name);
      let results: ReturnType<typeof runRigFile> | undefined;
      for (const caseName of cases)
        it(caseName, async () => {
          results ??= runRigFile(module, file);
          const mine = (await results).filter((r) => r.case === caseName);
          const bad = mine.filter((r) => !r.ok);
          expect(mine.length, `${caseName}: nothing was checked`).toBeGreaterThan(0);
          if (bad.length) expect.fail(bad.map((r) => `  [${r.device}] ${r.kind}: ${r.message}`).join('\n'));
        });
    });
  }
}
