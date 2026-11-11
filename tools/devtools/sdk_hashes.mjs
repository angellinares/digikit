// Byte-identity proof for the SDK restructure (docs/plans/sdk.md, S1).
//   node tools/devtools/sdk_hashes.mjs record  [FILE] [--force]   write the sha256 of every packed .dkm, every core object and the patched images
//   node tools/devtools/sdk_hashes.mjs compare [FILE] [--force]   build again, print every difference to FILE, exit 1 on any
// FILE defaults to out/sdk-hashes/baseline.json (out/ is ignored). Needs dist/patchkit/patchkit (`pnpm nx:pilot run patchkit:cli`).
// Steps: rebuild stale objects (--force: every object), pack every non-core module and fixture for each device it builds for
// (a pack that fails is recorded as "nopack" only for the known unpackable fixtures, otherwise fatal; so is any build failure or missing firmware), hash each .dkm file and each core object
// (without `source_sha256`, as tools/nx/device-bytes.mjs does), and hash the image the device patch set makes from each stock file.
import { execFileSync, execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { globSync, mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const pk = join(root, 'dist/patchkit/patchkit');
const work = join(root, 'dist/sdk-hashes');
const JOBS = 4;
const IMAGES = { dt2: 'firmware/Digitakt_II_OS1.17.syx', dn2: 'firmware/Digitone_II_OS1.12.syx' };

const [mode, ...rest] = process.argv.slice(2);
const force = rest.includes('--force');
const file = resolve(root, rest.find((a) => !a.startsWith('--')) ?? 'out/sdk-hashes/baseline.json');
if (!['record', 'compare'].includes(mode) || !existsSync(pk)) {
  console.error('usage: sdk_hashes.mjs record|compare [FILE] [--force]  (needs dist/patchkit/patchkit)');
  process.exit(2);
}

const sha = (buf) => createHash('sha256').update(buf).digest('hex');
const run = (args, opts = {}) =>
  execFileSync('nice', [pk, '--root', root, ...args], {
    cwd: root,
    stdio: ['ignore', 'pipe', 'pipe'],
    maxBuffer: 1 << 28,
    ...opts,
  });
const runAsync = (args) =>
  new Promise((res) =>
    execFile('nice', [pk, '--root', root, ...args], { cwd: root, maxBuffer: 1 << 28 }, (err, _o, stderr) =>
      res(err ? String(stderr).trim().split('\n')[0] || 'failed' : null),
    ),
  );

// modules that pack to a .dkm: not core, not a framework, dynamic, raw or service (as tools/device/dktransfer.py candidates)
const modules = globSync('{modules,tests/fixtures/modules}/**/module.toml', { cwd: root })
  .sort()
  .map((f) => ({ f, m: parse(readFileSync(join(root, f), 'utf8')).module }))
  .filter(
    ({ m }) =>
      !m.id.startsWith('core/') && m.type !== 'framework' && (m.dynamic || m.type === 'raw' || m.type === 'service'),
  );
const devicesOf = (m) =>
  m.ports?.length ? ['dt2', 'dn2'].filter((d) => m.ports.some((p) => p.startsWith(`${d}-`))) : ['dt2', 'dn2'];

// Any build failure, an unexpected nopack or a missing firmware image is fatal: a stale object would otherwise be hashed unchanged.
const NOPACK_OK = new Set(['test/cf-demo', 'test/registry-demo', 'test/rust-demo']);
const failures = [];

const t0 = Date.now();
const lap = (what) => console.error(`${((Date.now() - t0) / 1000).toFixed(1)} s ${what}`);

// 1. objects
const buildArgs = force ? ['--force'] : [];
for (const tree of ['modules/core', 'modules', 'tests/fixtures/modules']) {
  try {
    run(['build', ...buildArgs, '--tree', tree]);
  } catch (e) {
    failures.push(`build --tree ${tree} failed: ${String(e.stderr).trim().split('\n').slice(-3).join(' | ')}`);
  }
  lap(`build ${tree}`);
}

// 2. packs, JOBS at a time (the objects are built, so the jobs only read them)
const result = {
  head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root }).toString().trim(),
  dkm: {},
  core: {},
  image: {},
};
mkdirSync(work, { recursive: true });
const jobs = modules.flatMap(({ m }) => devicesOf(m).map((d) => ({ id: m.id, d })));
let next = 0;
await Promise.all(
  Array.from({ length: JOBS }, async () => {
    while (next < jobs.length) {
      const { id, d } = jobs[next++];
      const out = join(work, `${d}-${id.replaceAll('/', '__')}.dkm`);
      const err = await runAsync(['pack', id, '--device', d, '-o', out]);
      if (err && !NOPACK_OK.has(id)) failures.push(`pack ${id} --device ${d} failed: ${err}`);
      result.dkm[`${d} ${id}`] = err ? 'nopack' : sha(readFileSync(out));
    }
  }),
);
lap(`pack ${jobs.length} jobs`);

// 3. core objects
for (const f of globSync('modules/core/**/build/*.dko.json', { cwd: root }).sort()) {
  const o = JSON.parse(readFileSync(join(root, f), 'utf8'));
  delete o.source_sha256;
  result.core[f] = sha(JSON.stringify(o));
}

// 4. patched images (device patch set on the stock firmware)
for (const [d, fw] of Object.entries(IMAGES)) {
  if (!existsSync(join(root, fw))) {
    failures.push(`missing firmware image ${fw}`);
    continue;
  }
  const out = join(work, `${d}-image.syx`);
  try {
    run(['apply', '--firmware', fw, '-o', out]);
    result.image[d] = sha(readFileSync(out));
  } catch (e) {
    failures.push(`apply ${d} failed: ${String(e.stderr).trim().split('\n').slice(-1)}`);
  }
}
lap('images');

if (failures.length) {
  for (const f of failures) console.error(`FATAL ${f}`);
  console.error(`${failures.length} failure(s): the hashes would not prove anything`);
  process.exit(1);
}

const count = (r) =>
  `${Object.values(r.dkm).filter((h) => h !== 'nopack').length} .dkm (${Object.values(r.dkm).filter((h) => h === 'nopack').length} nopack), ${Object.keys(r.core).length} core objects, ${Object.keys(r.image).length} images`;
if (mode === 'record') {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(result, null, 2)}\n`);
  console.log(`recorded ${file} at ${result.head.slice(0, 8)}: ${count(result)}`);
} else {
  const base = JSON.parse(readFileSync(file, 'utf8'));
  let diffs = 0;
  for (const kind of ['dkm', 'core', 'image']) {
    for (const key of new Set([...Object.keys(base[kind]), ...Object.keys(result[kind])])) {
      if (base[kind][key] === result[kind][key]) continue;
      diffs++;
      console.log(`DIFF ${kind} ${key}: ${base[kind][key] ?? 'missing'} -> ${result[kind][key] ?? 'missing'}`);
    }
  }
  console.log(
    `compared with ${file} (${base.head.slice(0, 8)}) at ${result.head.slice(0, 8)}: ${count(result)}; ${diffs} differences`,
  );
  process.exit(diffs ? 1 : 0);
}
