#!/usr/bin/env node
// Regenerates the README's screenshots and the image table between the README markers.
// The device screens come from apps/web/public/images (recaptured with --device, about 2.5 min, boots the emulator).
// The workbench screens come from the UI preview through tools/devtools/ui_shots.mjs: a full image and a half-size thumbnail each.
//
//   node tools/devtools/readme_shots.mjs [--device]
import fs from 'node:fs';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(fileURLToPath(import.meta.url), '../../..');
const previewUrl = 'http://127.0.0.1:5199/';
const START = '<!-- readme-shots:start -->';
const END = '<!-- readme-shots:end -->';

const SHOTS = [
  {
    file: 'apps/web/public/images/device-modules.png',
    alt: 'The MODULES list on the device',
    caption: 'Pick which modules load',
    kind: 'device',
  },
  {
    file: 'apps/web/public/images/device-digikit.png',
    alt: 'The DIGIKIT menu on the device',
    caption: 'About, health, memory, modules',
    kind: 'device',
  },
  {
    file: 'docs/images/workbench-emulator.png',
    scene: 'readme-emulator',
    alt: 'The workbench emulator page',
    caption: 'The real firmware on your computer',
    kind: 'scene',
  },
  {
    file: 'docs/images/workbench-transfer.png',
    scene: 'readme-transfer',
    alt: 'The workbench Transfer page',
    caption: 'Install modules over USB',
    kind: 'scene',
  },
  {
    file: 'docs/images/workbench-remote.png',
    scene: 'readme-remote',
    alt: 'The workbench Remote page',
    caption: 'Press keys and turn encoders from your computer',
    kind: 'scene',
  },
];
for (const s of SHOTS) s.thumb = s.kind === 'device' ? s.file : s.file.replace(/\.png$/, '-thumb.png');

const abs = (p) => path.join(root, p);
const fail = (msg) => {
  console.error(`readme_shots: ${msg}`);
  process.exit(1);
};
const run = (cmd, args, cwd = root) => {
  const r = spawnSync(cmd, args, { cwd, stdio: 'inherit' });
  if (r.status !== 0) fail(`${cmd} ${args.join(' ')} failed`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const answers = async () => {
  try {
    await fetch(previewUrl);
    return true;
  } catch {
    return false;
  }
};

let preview = null;
const stopPreview = () => {
  if (preview && !preview.killed) preview.kill();
};
process.on('exit', stopPreview);
process.on('SIGINT', () => {
  stopPreview();
  process.exit(130);
});

async function main() {
  if (process.argv.includes('--device')) run('node', ['apps/web/scripts/site-shots.mjs']);
  for (const s of SHOTS) {
    if (s.kind === 'device' && !fs.existsSync(abs(s.file)))
      fail(`${s.file} is missing; run with --device to capture it`);
  }
  if (!fs.existsSync(abs('apps/web/public/modules/index.json'))) {
    run('node', ['scripts/gen-modules.mjs'], abs('apps/web'));
  }

  if (!(await answers())) {
    preview = spawn('pnpm', ['--filter', '@digi/desktop', 'exec', 'vite', '--config', 'ui/preview/vite.config.ts'], {
      cwd: root,
      stdio: 'ignore',
    });
    let up = false;
    for (let t = 0; t < 120 && !up; t++) {
      if (preview.exitCode !== null) fail('the preview server exited');
      up = await answers();
      if (!up) await sleep(500);
    }
    if (!up) fail(`the preview server did not answer at ${previewUrl} within 60 s`);
  }

  const scenes = SHOTS.filter((s) => s.kind === 'scene');
  const names = scenes.map((s) => s.scene);
  const tmp = abs('out/runs/readme-shots');
  const full = path.join(tmp, 'full');
  const thumb = path.join(tmp, 'thumb');
  fs.mkdirSync(full, { recursive: true });
  fs.mkdirSync(thumb, { recursive: true });
  run('node', ['tools/devtools/ui_shots.mjs', '--out', full, '--scale', '2', ...names]);
  run('node', ['tools/devtools/ui_shots.mjs', '--out', thumb, '--scale', '0.5', ...names]);

  fs.mkdirSync(abs('docs/images'), { recursive: true });
  for (const s of scenes) {
    for (const [dir, dest] of [
      [full, s.file],
      [thumb, s.thumb],
    ]) {
      const src = path.join(dir, `${s.scene}.png`);
      if (!fs.existsSync(src)) fail(`${src} was not written`);
      fs.copyFileSync(src, abs(dest));
      console.log(`wrote ${dest}`);
    }
  }

  const cell = (s) =>
    `    <td align="center" valign="top"><a href="${s.file}"><img src="${s.thumb}" alt="${s.alt}" width="${s.kind === 'device' ? 256 : 280}"></a><br><sub>${s.caption}</sub></td>`;
  const table = (list) => `<table>\n  <tr>\n${list.map(cell).join('\n')}\n  </tr>\n</table>`;
  const tables = `${table(SHOTS.slice(0, 2))}\n\n${table(SHOTS.slice(2))}`;

  const readme = abs('README.md');
  const text = fs.readFileSync(readme, 'utf8');
  const a = text.indexOf(START);
  const b = text.indexOf(END);
  if (a < 0 || b < a) fail('README.md is missing the readme-shots markers');
  fs.writeFileSync(readme, `${text.slice(0, a + START.length)}\n${tables}\n${text.slice(b)}`);
  console.log('wrote README.md');
}

main().then(
  () => {
    stopPreview();
  },
  (e) => {
    stopPreview();
    fail(e?.message ?? String(e));
  },
);
