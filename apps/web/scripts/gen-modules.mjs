// Build step of the patch gallery: writes public/modules/bundle.json (what patchkit.wasm reads: ports + modules as
// texts) from the COMMITTED devices/ports/, modules/ and tests/fixtures/modules/. Never reads out/modules (generated oracles hold firmware
// words). The bundle equals `tools/patchkit/patchkit.py bundle` (the node test compares them).
//
// The objects of content modules (every module but category "core") are build output, not committed: `patchkit build` makes them
// first (it needs the m68k-elf toolchain; nothing is built when they are current) and they are read from the cache out/cache/dko/<id>/<src12>/ (`patchkit build MODDIR --paths` names the files).
// It also writes what the Transfer page ships (see shipModules): per device the prebuilt .dkm of each run-time content module
// (public/modules/<dev>/<leaf>.dkm, the bytes `mise run module:pack-all` writes), public/modules/index.json and the sources of the
// modules (public/modules/src/<id>/: module.toml, README.md, src/**; nothing of build/ or out/).
//
// The objects of `core/*` modules are committed in the module. The browser never builds: patchkit.wasm packs from the bundle.
//
//   node scripts/gen-modules.mjs [--out DIR]
import { readFileSync, readdirSync, writeFileSync, mkdirSync, existsSync, statSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, dirname, resolve, sep, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { parse } from 'smol-toml';
import { isShipped, shippedDevices } from '../src/data/shipped.mjs';

const here = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(here, '../../..');

const walk = (dir, name, out = []) => {
  if (!existsSync(dir)) return out;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, e.name);
    if (e.isDirectory()) walk(full, name, out);
    else if (e.name === name) out.push(full);
  }
  return out;
};
// Python's sorted(Path) compares path parts, not the joined string.
const byParts = (a, b) => {
  const x = a.split(sep),
    y = b.split(sep);
  for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return x.length - y.length;
};

const GALLERY = 'build/gen/gallery.json';

/** `patchkit build` over the repository's module roots: builds the stale objects of content modules into the cache. */
export function buildObjects(root = repoRoot) {
  const env = { ...process.env, PYTHONPATH: root };
  delete env.PATCHKIT_MODULES; // only the committed roots go into the bundle
  try {
    execFileSync('python3', ['-m', 'tools.patchkit.patchkit', 'build'], {
      cwd: root,
      env,
      encoding: 'utf8',
      stdio: 'pipe',
    });
  } catch (e) {
    throw new Error(
      `patchkit build failed (the gallery needs the module objects; toolchain: uv run python -m tools.patchkit.pk_toolchains m68k):\n${e.stderr || e.message}`,
    );
  }
}

/** `patchkit build MODDIR --paths`: the stored file of each object with sources, for the sources now (the cache entry is keyed by them). */
function cachedPaths(root, dir) {
  const env = { ...process.env, PYTHONPATH: root };
  delete env.PATCHKIT_MODULES;
  const out = execFileSync('python3', ['-m', 'tools.patchkit.patchkit', 'build', dir, '--paths'], {
    cwd: root,
    env,
    encoding: 'utf8',
    stdio: 'pipe',
  });
  return Object.values(JSON.parse(out))[0] ?? {};
}

/** The text of an object: in the module for `core/*`, else in the cache (`stored`: the paths of `cachedPaths`). */
const objectText = (dir, d, o, stored) =>
  readFileSync(
    d.module.category !== 'core' && (o.sources ?? []).length > 0 ? stored[o.path] : join(dir, o.path),
    'utf8',
  );

/** `{bundle}` from the committed tree under `root` plus the built objects of the content modules. */
export function buildModuleData(root = repoRoot) {
  buildObjects(root);
  const ports = readdirSync(join(root, 'devices', 'ports'))
    .filter((f) => f.endsWith('.toml'))
    .sort()
    .map((f) => readFileSync(join(root, 'devices', 'ports', f), 'utf8'));
  // the module roots in the order of tools/patchkit/modroots.py REPO_ROOTS; the tests use the fixtures
  const manifests = ['modules', join('tests', 'fixtures', 'modules')].flatMap((r) =>
    walk(join(root, r), 'module.toml').sort(byParts),
  );
  const modules = [];
  for (const file of manifests) {
    const dir = dirname(file);
    const text = readFileSync(file, 'utf8');
    const d = parse(text);
    const files = {};
    const stored =
      d.module.category !== 'core' && (d.object ?? []).some((o) => (o.sources ?? []).length > 0)
        ? cachedPaths(root, dir)
        : {};
    for (const o of d.object ?? []) files[o.path] = objectText(dir, d, o, stored);
    for (const r of d.registry ?? [])
      if (r.index_table) files[r.index_table] = readFileSync(join(dir, r.index_table), 'utf8');
    // `patchkit gen`: machines and parameters, for the page
    if (existsSync(join(dir, GALLERY))) files[GALLERY] = readFileSync(join(dir, GALLERY), 'utf8');
    modules.push({ manifest: text, files });
  }
  // the device files with a [modules] section: the knob models the typed module sections are lowered with
  const devices = readdirSync(join(root, 'devices'))
    .filter((f) => f.endsWith('.toml'))
    .sort()
    .map((f) => readFileSync(join(root, 'devices', f), 'utf8'))
    .filter((t) => 'modules' in parse(t));
  return { bundle: { ports, modules, devices } };
}

const SOURCE_MAX = 256 * 1024;

/** The files of a module that the site shows: module.toml, README.md and src/** (text only), relative to the module, sorted. */
function sourceFiles(dir) {
  const out = ['module.toml', 'README.md'].filter((f) => existsSync(join(dir, f)));
  const walkSrc = (d) => {
    for (const e of readdirSync(join(dir, d), { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const rel = `${d}/${e.name}`;
      if (e.isDirectory()) walkSrc(rel);
      else if (e.isFile()) out.push(rel);
    }
  };
  if (existsSync(join(dir, 'src'))) walkSrc('src');
  return out;
}

const sha256 = (b) => createHash('sha256').update(b).digest('hex');

/** `patchkit pack` of one module for one device (the packer `mise run module:pack-all` uses); null when it does not pack for the device. */
function pack(root, id, device, to) {
  const env = { ...process.env, PYTHONPATH: root };
  delete env.PATCHKIT_MODULES;
  try {
    execFileSync('python3', ['-m', 'tools.patchkit.patchkit', 'pack', id, '--device', device, '-o', to], {
      cwd: root,
      env,
      encoding: 'utf8',
      stdio: 'pipe',
    });
    return readFileSync(to);
  } catch {
    return null;
  }
}

/** Write the shipped modules under `out`: <dev>/<leaf>.dkm, index.json and src/<id>/**. Returns the index. */
export function shipModules(out, root = repoRoot) {
  const scratch = join(root, 'out', 'cache', 'site-pack');
  for (const sub of ['dt2', 'dn2', 'src']) rmSync(join(out, sub), { recursive: true, force: true });
  const modules = [];
  const manifests = ['machines', 'fx', 'tools'].flatMap((r) =>
    walk(join(root, 'modules', r), 'module.toml').sort(byParts),
  );
  for (const file of manifests) {
    const dir = dirname(file);
    const d = parse(readFileSync(file, 'utf8'));
    if (!isShipped(d, relative(root, dir).split(sep).join('/'))) continue;
    const m = d.module;
    const leaf = m.id.split('/').pop();
    const devices = {};
    for (const dev of shippedDevices(d)) {
      const tmp = join(scratch, dev, `${leaf}.dkm`);
      mkdirSync(dirname(tmp), { recursive: true });
      const bytes = pack(root, m.id, dev, tmp);
      if (!bytes) continue;
      mkdirSync(join(out, dev), { recursive: true });
      writeFileSync(join(out, dev, `${leaf}.dkm`), bytes);
      devices[dev] = { file: `${dev}/${leaf}.dkm`, size: bytes.length, sha256: sha256(bytes) };
    }
    if (!Object.keys(devices).length) continue;
    const files = sourceFiles(dir);
    for (const f of files) {
      const bytes = readFileSync(join(dir, f));
      if (bytes.length > SOURCE_MAX) throw new Error(`${m.id}/${f} is too large to show (${bytes.length} bytes)`);
      const to = join(out, 'src', m.id, f);
      mkdirSync(dirname(to), { recursive: true });
      writeFileSync(to, bytes);
    }
    modules.push({
      id: m.id,
      name: m.name,
      summary: m.summary,
      kind: m.category,
      version: m.version,
      devices,
      source: files,
    });
  }
  const index = { modules };
  writeFileSync(join(out, 'index.json'), JSON.stringify(index, null, 1));
  return index;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const i = process.argv.indexOf('--out');
  const out = i > 0 ? resolve(process.argv[i + 1]) : join(here, '..', 'public', 'modules');
  const { bundle } = buildModuleData();
  mkdirSync(out, { recursive: true });
  writeFileSync(join(out, 'bundle.json'), JSON.stringify(bundle));
  const index = shipModules(out);
  console.log(`shipped: ${index.modules.map((m) => `${m.id} (${Object.keys(m.devices).join(',')})`).join(', ')}`);
  console.log(
    `modules: ${bundle.modules.length} modules, ${bundle.ports.length} ports, bundle ${statSync(join(out, 'bundle.json')).size} bytes`,
  );
}
