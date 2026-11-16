import fs from 'node:fs';
import path from 'node:path';
import { LOAD_STATUS } from '@digi/dk-session/gen_proto';
import { expect, test } from 'vitest';
import { ROWS, type Proof } from '../../../modules/core/runtime/tests/refusal/refusal-table.ts';

const root = path.resolve(import.meta.dirname, '../../..');
const text = (file: string): string => {
  const at = path.join(root, file);
  expect(fs.existsSync(at), `${file} exists`).toBe(true);
  return fs.readFileSync(at, 'utf8');
};
const names = LOAD_STATUS.map(([, name]) => name);

test('every load status has a row, and every row names a load status', () => {
  expect(Object.keys(ROWS).sort()).toEqual([...names].sort());
});

test('a row either proves the code with a must-fail and a must-pass test, or says why it cannot', () => {
  for (const [code, row] of Object.entries(ROWS)) {
    if ('fail' in row) {
      for (const p of [row.fail, row.pass])
        expect(text(p.file), `${code}: ${p.file} has the test "${p.test}"`).toContain(p.test);
      // a test that proves the code names it, by name or by number, in its file or in the module synthesizer it builds its fixtures with
      const number = LOAD_STATUS.find(([, n]) => n === code)![0];
      const src = text(row.fail.file) + text('tests/regression/runtime/synth.ts');
      expect(src.includes(code) || src.includes(String(number)), `${code}: ${row.fail.file} names the code`).toBe(true);
    } else {
      expect(row.reason.length, `${code}: a reason`).toBeGreaterThan(10);
      for (const p of row.covered ?? [])
        expect(text(p.file), `${code}: ${p.file} mentions ${p.test}`).toContain(p.test);
    }
  }
});

test('a module-file fixture is never marked reserved or not-a-load for a code a file can cause', () => {
  // the codes the loader and the boot plan give a module of the selection: each has a fixture or is listed as open
  const fileCodes = [
    'E_NOT_FOUND',
    'E_SHA',
    'E_FORMAT',
    'E_IMPORT',
    'E_NOMEM',
    'E_LOADED',
    'E_TOO_BIG',
    'E_DECL_ID',
    'E_DECL_USES',
    'E_FX_ID_DUP',
    'E_PLAN_KIND',
    'E_PLAN_KEY',
    'E_PLAN_FIT',
    'E_INIT',
    'E_DECL_KIND',
    'E_CONTRACT',
    'E_MISSING',
    'E_SVC_ABI',
    'E_DSP_NOSPACE',
    'E_DSP_ROWS',
    'E_SAFE_MODE',
  ];
  for (const code of fileCodes)
    expect('fail' in ROWS[code]!, `${code} has a must-fail and a must-pass fixture`).toBe(true);
});

test('the open rows are the known gaps', () => {
  const open = Object.entries(ROWS)
    .filter(([, r]) => 'kind' in r && r.kind === 'open')
    .map(([c]) => c);
  // a gap that gets a fixture leaves this list; a new gap is a decision, not an accident
  expect(open.sort()).toEqual(
    [
      'E_BOOT_ONLY',
      'E_CARD_BUSY',
      'E_DSP_IMPORT',
      'E_NO_REGISTRY',
      'E_READ',
      'E_REG_DEVICE',
      'E_SEL_WRITE',
      'E_TABLE',
    ].sort(),
  );
});

test('a proof names a test of its own file, not the table', () => {
  const proofs: Proof[] = Object.values(ROWS).flatMap((r) => ('fail' in r ? [r.fail, r.pass] : (r.covered ?? [])));
  for (const p of proofs) expect(p.file).not.toContain('refusal-table');
});
