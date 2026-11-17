/**
 * The Vitest entry of a module's sound checks: `rigSuite(import.meta.dirname)` in modules/<kind>/<name>/tests/rig.rig.test.ts runs every `rig/*.toml` there.
 * With `DIGI_RIG_PIN=1` it runs no checks: it writes the `same-as` pins, the fingerprints and the local cache of the pinned output (the re-pin step).
 * With `DIGI_RIG_REPORT=1` it runs no checks: it writes `out/rig/<case>/{out.wav,ref.wav,plot.png}` and prints one verdict per case (docs/TESTING.md "Classifying a changed pin").
 */
import { readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { loadRigFile, pinRigFile, reportRigFile, runRigFile } from './soundcheck.ts';
import { moduleIdOfDir } from './spec.ts';

export function rigSuite(testsDir: string): void {
  const module = moduleIdOfDir(path.dirname(testsDir));
  const dir = path.join(testsDir, 'rig');
  for (const name of readdirSync(dir)
    .filter((f) => f.endsWith('.toml'))
    .sort()) {
    const file = path.join(dir, name);
    if (process.env.DIGI_RIG_REPORT === '1') {
      it(`${module} ${name}: report`, async () => {
        const text = (await reportRigFile(module, file)).join('\n');
        writeFileSync(
          path.join(process.env.DIGI_RIG_REPORT_FILE ?? path.join(tmpdir(), 'rig-report.txt')),
          `${text}\n`,
          { flag: 'a' },
        );
        console.log(text);
      });
      continue;
    }
    if (process.env.DIGI_RIG_PIN === '1') {
      it(`${module} ${name}: write the pins`, async () => {
        for (const line of await pinRigFile(module, file)) console.log(`pin ${line}`);
      });
      continue;
    }
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
