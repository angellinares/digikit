// Captures the device screens used by the site and the README: boots DT2 in the coupled emulator with the shipped
// modules on the card, opens the DIGIKIT menu and the MODULES list, and copies both images into public/images.
// --list prints the module list and does not start the emulator.
import { readFileSync, readdirSync, existsSync, mkdirSync, copyFileSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { parse } from 'smol-toml';
import { isShipped, shippedDevices } from '../src/data/shipped.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

const ids = [];
for (const kind of ['machines', 'fx', 'tools']) {
  const base = join(root, 'modules', kind);
  if (!existsSync(base)) continue;
  for (const e of readdirSync(base, { withFileTypes: true })) {
    const file = join(base, e.name, 'module.toml');
    if (!e.isDirectory() || !existsSync(file)) continue;
    const manifest = parse(readFileSync(file, 'utf8'));
    const dir = `modules/${kind}/${e.name}`;
    if (!isShipped(manifest, dir) || !shippedDevices(manifest).includes('dt2')) continue;
    ids.push(manifest.module.id);
  }
}
const leaf = (id) => id.split('/').pop().toLowerCase();
ids.sort((a, b) => (leaf(a) < leaf(b) ? -1 : leaf(a) > leaf(b) ? 1 : 0));
const store = ids.map((id, i) => (i % 3 === 0 ? `${id}*` : id)).join(',');

if (process.argv.includes('--list')) {
  console.log(store);
  process.exit(0);
}

const base = ['run', 'python', '-m', 'tools.patchkit.pk_devreload', 'emulator', '--device', 'dt2', '--store', store];
const keys =
  'tap:NO,wait:300,tap:SETTINGS,wait:400,tap:DOWN*8,wait:150,tap:YES,wait:500,shot:digikit,tap:DOWN*3,tap:YES,wait:800,shot:modules';
const run = (args) => execFileSync('uv', [...base, ...args], { cwd: root, stdio: 'inherit' });

try {
  run(['--keys', keys]);
} finally {
  run(['--stop']);
}

const shots = join(root, 'out', 'runs', 'devreload', 'dt2');
const images = join(root, 'apps', 'web', 'public', 'images');
mkdirSync(images, { recursive: true });
copyFileSync(join(shots, 'modules.png'), join(images, 'device-modules.png'));
copyFileSync(join(shots, 'digikit.png'), join(images, 'device-digikit.png'));
