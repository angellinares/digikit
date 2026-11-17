// What a .dkm takes of the device: patchkit.wasm (`pk_needs`, packages/dkneeds) and the patchkit command line (`patchkit needs --json`) run the
// same Rust code on real packed modules and must agree to the field; the wasm reader refuses what is not a .dkm without a crash. The arithmetic itself
// is proved against the runtime in the scenarios `inferred-needs-*` and by the Rust unit tests of dkneeds. Skips when patchkit.wasm is not built.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'vitest';
import { H_ROLE, TABLE_VERSION } from '@digi/dk-session';
import { groupOf } from '@digi/workbench-core/modules/dktransfer.ts';
import { DkNeeds } from '@digi/workbench-core/modules/dkneeds.ts';
import type { Needs } from '@digi/workbench-core/modules/fit.ts';
import { py, readBytes, wasmPath, workDir } from './support/patchkit-env.ts';

const patchkit = (...args: string[]) => py('tools.patchkit.patchkit', ...args);
const MODULES = ['test/sine', 'digikit/remote', 'test/dyn-dn2', 'test/dyn-demo'];

test.skipIf(!existsSync(wasmPath))(
  'the wasm reader and the command line give the same needs for packed modules',
  async () => {
    const work = workDir('dkneeds-');
    const reader = await DkNeeds.create(readBytes(wasmPath));
    const files = MODULES.map((id) => {
      const path = join(work, `${id.replace('/', '-')}.dkm`);
      patchkit('pack', id, '--device', 'dt2', '-o', path);
      return path;
    });
    const cli = JSON.parse(patchkit('needs', '--json', ...files));
    const seen: Record<string, Required<Needs>> = {};
    for (const [id, path] of MODULES.map((id, i) => [id, files[i]])) {
      const bytes = new Uint8Array(readFileSync(path));
      const n = reader.of(bytes)! as Required<Needs>;
      assert.deepEqual(n, cli[path], id);
      assert.equal(n.file_size, bytes.length, id);
      seen[id] = n;
    }
    // what the modules are: a machine with DSP code takes a DSP range, a block for its relocated image, a voice row and a machine row
    const sine = seen['test/sine'];
    assert.ok(sine.DSP_AREA > 0 && sine.DSP_AREA % 64 === 0, 'the DSP range is whole granules');
    assert.ok(sine.ARENA_XFER > 0 && sine.VOICE_ROWS >= 1 && sine.MACHINE_ROWS >= 1);
    assert.equal(sine.ARENA % 16, 0);
    // a tool with no DSP code and no records takes the arena block only
    const remote = seen['digikit/remote'];
    assert.deepEqual([remote.DSP_AREA, remote.ARENA_XFER, remote.VOICE_ROWS, remote.MACHINE_ROWS], [0, 0, 0, 0]);
    assert.ok(remote.ARENA > 0);
  },
);

test.skipIf(!existsSync(wasmPath))('what is not a .dkm has no needs and does not crash the reader', async () => {
  const reader = await DkNeeds.create(readBytes(wasmPath));
  for (const bytes of [
    new Uint8Array(0),
    new Uint8Array(0xa0),
    new Uint8Array(4096).fill(0xff),
    Uint8Array.of(1, 2, 3),
  ]) {
    assert.equal(reader.of(bytes), null);
    assert.match((reader.read(bytes) as { error: string }).error, /not a \.dkm/);
  }
  // a header-sized file with the right magic, format and size fields but nothing else: the sizes do not fit
  const b = new Uint8Array(0xa0);
  const v = new DataView(b.buffer);
  v.setUint32(0, 0x444b4d46);
  v.setUint32(4, 1);
  v.setUint32(8, 0xa0);
  v.setUint32(0x0c, 0xa0);
  assert.match((reader.read(b) as { error: string }).error, /sizes/);
  // the reader still works after refusing files
  assert.equal(reader.of(new Uint8Array(10)), null);
});

test.skipIf(!existsSync(wasmPath))(
  'a packed service is a version 2 table with role 1, grouped as Services, and its user is a version 1 module with a need; the reader reads both',
  async () => {
    const work = workDir('dkneeds-svc-');
    const svc = join(work, 'svc-test.dks');
    const user = join(work, 'svc-test-user.dkm');
    patchkit('pack', 'test/svc-test', '--device', 'dt2', '-o', svc);
    patchkit('pack', 'test/svc-test-user', '--device', 'dt2', '-o', user);
    const [s, u] = [svc, user].map((p) => new Uint8Array(readFileSync(p)));
    // the table follows the header (H_REG_OFF, word 0x..): its magic, then its u16 version
    const version = (f: Uint8Array) => {
      const v = new DataView(f.buffer, f.byteOffset, f.length);
      const off = v.getUint32(0x90);
      return v.getUint16(off + 4);
    };
    assert.deepEqual([version(s), version(u)], [2, 1]);
    assert.ok(version(s) <= TABLE_VERSION);
    assert.deepEqual(
      [s, u].map((f) => new DataView(f.buffer, f.byteOffset, f.length).getUint32(H_ROLE)),
      [1, 0],
    );
    assert.equal(groupOf(s), 'Services');
    assert.notEqual(groupOf(u), 'Services');
    // the wasm reader and the command line agree on a service file too
    const reader = await DkNeeds.create(readBytes(wasmPath));
    const cli = JSON.parse(patchkit('needs', '--json', svc, user));
    assert.deepEqual(reader.of(s), cli[svc]);
    assert.deepEqual(reader.of(u), cli[user]);
  },
);
