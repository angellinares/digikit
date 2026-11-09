import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test, vi } from 'vitest';
import { BUTTONS, button, encoder, golden, validateRuntimeInputs, coupledEnv, Session } from '../src/index.ts';

test('empty startup rejects sessions that cannot defer a reusable coupled baseline', async () => {
  for (const options of [
    { dsp: 'cold' as const, runtime: { reuse: true } },
    { dsp: 'zero' as const, runtime: { reuse: true } },
    { dsp: 'sharc' as const },
    { dsp: 'sharc' as const, runtime: {} },
  ]) {
    await assert.rejects(
      Session.start({ device: 'dn2', firmware: '1.12', ...options, startEmpty: true }),
      /empty startup requires a reusable coupled session/,
    );
  }
});

test('key names follow the Python harness table', () => {
  assert.equal(button('trig_1'), 25);
  assert.equal(button('Trig 16'), 40);
  assert.equal(button('push h'), 48);
  assert.equal(button('page trig'), 1);
  assert.equal(button(7), 7);
  assert.equal(encoder('enc a'), 1);
  assert.equal(encoder('LEVEL'), 9);
  assert.equal(Object.keys(BUTTONS).length, 26 + 16 + 8);
  assert.throws(() => button('nope'));
});

test('golden writes only under UPDATE_GOLDENS=1', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-golden-'));
  const file = path.join(dir, 'g.json');
  const prev = process.env.UPDATE_GOLDENS;
  try {
    delete process.env.UPDATE_GOLDENS;
    assert.throws(() => golden(file, 'a', 1));
    process.env.UPDATE_GOLDENS = '1';
    golden(file, 'a', { x: 1 });
    delete process.env.UPDATE_GOLDENS;
    golden(file, 'a', { x: 1 });
    assert.throws(() => golden(file, 'a', { x: 2 }));
  } finally {
    if (prev === undefined) delete process.env.UPDATE_GOLDENS;
    else process.env.UPDATE_GOLDENS = prev;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('runtime provenance accepts matching stock bytes and rejects forced or stale ready inputs', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-provenance-'));
  const syx = path.join(dir, 'stock.syx');
  fs.writeFileSync(syx, 'stock fixture');
  const inputs = { syx, readyDir: dir, cfSnapshot: '', dspImage: '', dspState: '' };
  const source = { syx_sha256: createHash('sha256').update('stock fixture').digest('hex'), intc_force: false };
  const write = (value: object) => fs.writeFileSync(path.join(dir, 'SOURCE.json'), JSON.stringify(value));
  try {
    write(source);
    validateRuntimeInputs(inputs);
    write({ ...source, intc_force: true });
    assert.throws(() => validateRuntimeInputs(inputs), /provenance mismatch/);
    write({ ...source, syx_sha256: 'stale' });
    assert.throws(() => validateRuntimeInputs(inputs), /provenance mismatch/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('coupled launch defaults follow actual firmware bytes and preserve explicit overrides', () => {
  const dt2 = Buffer.from('stock DT2 fixture');
  const dn2 = Buffer.from('stock DN2 fixture');
  const sha = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
  const table = JSON.stringify({
    [sha(dt2)]: { fast_regions: 'dt2-regions', blocktrans: 'dt2-roots', fallback_regions: 'dt2-fallback' },
    [sha(dn2)]: { fast_regions: 'dn2-regions', blocktrans: 'dn2-roots' },
  });
  const read = vi.spyOn(fs, 'readFileSync');
  const firmware = (bytes: Buffer) => read.mockReturnValueOnce(bytes).mockReturnValueOnce(table);
  try {
    firmware(dt2);
    assert.deepEqual(coupledEnv('same-path.syx', {}), {
      SHARC_FAST_REGIONS: 'dt2-regions',
      SHARC_BLOCKTRANS: 'dt2-roots',
      SHARC_FAST_FALLBACK_REGIONS: 'dt2-fallback',
    });
    for (const explicit of ['', 'ordinary-roots']) {
      firmware(dt2);
      assert.deepEqual(coupledEnv('dt2.syx', { SHARC_FAST_REGIONS: explicit }), {
        SHARC_FAST_REGIONS: explicit,
        SHARC_BLOCKTRANS: 'dt2-roots',
      });
    }
    for (const fallback of ['', 'explicit-fallback']) {
      firmware(dt2);
      assert.deepEqual(
        coupledEnv('dt2.syx', {
          SHARC_FAST_REGIONS: '',
          SHARC_FAST_FALLBACK_REGIONS: fallback,
        }),
        {
          SHARC_FAST_REGIONS: '',
          SHARC_BLOCKTRANS: 'dt2-roots',
          SHARC_FAST_FALLBACK_REGIONS: fallback,
        },
      );
    }
    firmware(dt2);
    assert.deepEqual(coupledEnv('dt2.syx', { SHARC_FAST_AUTO: '10,100' }), {
      SHARC_FAST_REGIONS: 'dt2-regions',
      SHARC_BLOCKTRANS: 'dt2-roots',
    });
    firmware(dn2); // Replacing bytes at the same path must not reuse DT2's defaults.
    assert.deepEqual(coupledEnv('same-path.syx', {}), {
      SHARC_FAST_REGIONS: 'dn2-regions',
      SHARC_BLOCKTRANS: 'dn2-roots',
    });
    firmware(dn2);
    assert.deepEqual(coupledEnv('dn2.syx', { SHARC_FAST_REGIONS: '', SHARC_BLOCKTRANS: 'explicit' }), {
      SHARC_FAST_REGIONS: '',
      SHARC_BLOCKTRANS: 'explicit',
    });
    firmware(Buffer.from('unknown firmware'));
    assert.deepEqual(coupledEnv('unknown.syx', {}), {});
    assert.deepEqual(coupledEnv('not-read.syx', { DIGI_EMU_FAST: '0' }), {});
  } finally {
    read.mockRestore();
  }
});
