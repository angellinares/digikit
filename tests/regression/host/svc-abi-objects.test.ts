// The committed objects of the service files bake the service ABI fingerprint (`dk_svc_abi`), and the runtime object compares a file's word with its own
// `SVC_ABI` (an immediate in `abi_ok`). A source change that moves `SVC_ABI` leaves the service objects stale (their sources did not change), and the loader then
// refuses every service file with E_SVC_ABI: rebuild them with `patchkit build modules/core/services/<name>/dks`.
import fs from 'node:fs';
import path from 'node:path';
import { expect, test } from 'vitest';

const root = path.resolve(import.meta.dirname, '../../..');
const object = (file: string) =>
  JSON.parse(fs.readFileSync(path.join(root, file), 'utf8')) as { sections: { name: string; hex: string }[] };

const abiOf = (file: string) => object(file).sections.find((s) => s.name === '.rodata.dk_svc_abi')!.hex;

test('the service objects carry the ABI fingerprint the runtime object compares with', () => {
  const runtime = object('modules/core/runtime/build/runtime.dko.json')
    .sections.map((s) => s.hex)
    .join('|');
  for (const name of ['fx', 'machines', 'lfo']) {
    const abi = abiOf(`modules/core/services/${name}/dks/build/${name}.dko.json`);
    // `cmpi.l #SVC_ABI,d0` in `RealRegistry::abi_ok`
    expect(runtime, `${name}.dko.json: rebuild it (patchkit build modules/core/services/${name}/dks)`).toContain(
      `0c80${abi}`,
    );
  }
});
