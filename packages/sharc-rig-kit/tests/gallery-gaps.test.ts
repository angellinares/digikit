import { expect, it } from 'vitest';
import { galleryGaps } from '../src/gallery-gaps.mjs';

const FILE = 'build/gen/gallery.json';
const manifest = { machine: [{ id: 'm' }], targets: { 'dt2-1.17': 'x', 'dn2-1.12': 'x' } };

it('reports a device the gallery lacks and a device without parameters', () => {
  const gallery = { machines: [{ id: 'm', devices: { dn2: {} } }], params: { dn2: [], dt2: [{ id: 'a' }] } };
  expect(galleryGaps(manifest, gallery, FILE)).toEqual([
    `dn2: machine m has no parameter in ${FILE}`,
    `dt2: machine m is not listed in ${FILE}`,
  ]);
});

it('accepts a device that declares param_rows = 0 and a module without [[machine]]', () => {
  const gallery = { machines: [{ id: 'm', devices: { dn2: {}, dt2: {} } }], params: { dn2: [], dt2: [{ id: 'a' }] } };
  expect(galleryGaps({ ...manifest, uses: { dn2: { param_rows: 0 } } }, gallery, FILE)).toEqual([]);
  expect(galleryGaps({ targets: manifest.targets }, { machines: [{ id: 'm', devices: {} }] }, FILE)).toEqual([]);
});
