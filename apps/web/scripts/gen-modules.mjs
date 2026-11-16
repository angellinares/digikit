// Build step of the patch gallery: writes public/modules/bundle.json (what patchkit.wasm reads: ports + modules as
// texts) from the COMMITTED devices/ports/, modules/ and tests/fixtures/modules/. Never reads out/modules (generated oracles hold firmware
// words). The bundle equals `tools/patchkit/patchkit.py bundle` (the node test compares them).
//
// The objects of content modules (every module but category "core") are build output, not committed: `patchkit build` (the Rust CLI, run through cargo) makes them
// first (it needs the m68k-elf toolchain; nothing is built when they are current) and they are read from the cache out/cache/dko/<id>/<src12>/ (`patchkit build MODDIR... --paths` names the files).
// It also writes what the Transfer page ships (see shipModules): per device the prebuilt .dkm of each run-time content module
// (public/modules/<dev>/<leaf>.dkm, the bytes the workbench app's `lib_build` writes to out/modules/<dev>/), public/modules/index.json and the sources of the
// modules (public/modules/src/<id>/: module.toml, README.md, src/**; nothing of build/ or out/).
//
// The objects of `core/*` modules are committed in the module and are only read here, never rebuilt. The browser never builds: patchkit.wasm packs from the bundle.
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

/**
 * `patchkit build DIR...` over the given content-module directories: builds their stale objects into the cache.
 * Core modules are never passed: their objects are committed and read as they are (`patchkit:objects-check` finds a stale one).
 */
export function buildObjects(dirs, root = repoRoot) {
  if (!dirs.length) return;
  try {
    patchkit(['build', ...dirs], { root });
  } catch (e) {
    throw new Error(
      `patchkit build failed (the gallery needs the module objects; toolchain: uv run python -m tools.patchkit.pk_toolchains m68k):\n${e.message}`,
    );
  }
}

/** Runs the Rust patchkit CLI from the repository `root`; returns stdout, throws an Error with the stderr tail on failure. */
function patchkit(args, { root = repoRoot } = {}) {
  const env = { ...process.env };
  delete env.PATCHKIT_MODULES; // only the committed roots go into the bundle
  try {
    return execFileSync(
      'cargo',
      ['run', '-q', '--release', '--offline', '-p', 'patchkit', '--', '--root', root, ...args],
      {
        cwd: root,
        env,
        encoding: 'utf8',
        stdio: 'pipe',
        maxBuffer: 256 * 1024 * 1024,
      },
    );
  } catch (e) {
    const err = new Error(
      String(e.stderr ?? e.message ?? e)
        .trim()
        .split('\n')
        .slice(-40)
        .join('\n'),
    );
    err.stdout = String(e.stdout ?? '');
    throw err;
  }
}

/** `patchkit build DIR... --paths`: per directory, the stored file of each object with sources, for the sources now (the cache entry is keyed by them). */
function cachedPaths(root, dirs) {
  if (!dirs.length) return {};
  return JSON.parse(patchkit(['build', ...dirs, '--paths'], { root }));
}

/** The text of an object: in the module for `core/*`, else in the cache (`stored`: the paths of `cachedPaths`). */
const objectText = (dir, d, o, stored) =>
  readFileSync(
    d.module.category !== 'core' && (o.sources ?? []).length > 0 ? stored[o.path] : join(dir, o.path),
    'utf8',
  );

/** `{bundle}` from the committed tree under `root` plus the built objects of the content modules. */
export function buildModuleData(root = repoRoot) {
  const ports = readdirSync(join(root, 'devices', 'ports'))
    .filter((f) => f.endsWith('.toml'))
    .sort()
    .map((f) => readFileSync(join(root, 'devices', 'ports', f), 'utf8'));
  // the module roots in the order of tools/patchkit/modroots.py REPO_ROOTS; the tests use the fixtures
  // The published site leaves out the test fixtures: only the site's tests use them, and some need the Rust toolchain for ColdFire.
  const roots = process.env.DIGI_SITE_NO_FIXTURES ? ['modules'] : ['modules', join('tests', 'fixtures', 'modules')];
  const manifests = roots.flatMap((r) => walk(join(root, r), 'module.toml').sort(byParts));
  const parsed = manifests.map((file) => {
    const text = readFileSync(file, 'utf8');
    return { dir: dirname(file), text, d: parse(text) };
  });
  const built = (d) => d.module.category !== 'core' && (d.object ?? []).some((o) => (o.sources ?? []).length > 0);
  const builtDirs = parsed.filter((m) => built(m.d)).map((m) => m.dir);
  buildObjects(builtDirs, root);
  const storedByDir = cachedPaths(root, builtDirs);
  const modules = [];
  for (const { dir, text, d } of parsed) {
    const files = {};
    const stored = built(d) ? (storedByDir[dir] ?? {}) : {};
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

/** The texts of the `devices/fw/` declaration files (`subsystems.toml` at the top left out), in the order `patchkit::fwgen::fw_texts` reads them. */
function fwTexts(root, dir = join(root, 'devices', 'fw'), top = true) {
  const out = [];
  const byName = (x, y) => (x.name < y.name ? -1 : x.name > y.name ? 1 : 0);
  for (const e of readdirSync(dir, { withFileTypes: true }).sort(byName)) {
    const full = join(dir, e.name);
    if (e.isDirectory()) out.push(...fwTexts(root, full, false));
    else if (e.name.endsWith('.toml') && !(top && e.name === 'subsystems.toml')) out.push(readFileSync(full, 'utf8'));
  }
  return out;
}

/**
 * `{ports, fw}`: what `pk_bind` reads besides the stock image to bind a service file (the Transfer page does it on the user's machine). It holds no module
 * and no firmware: the ports are the hashes and hook sites, `fw` the argument plan of each hook.
 */
export function buildBindBundle(root = repoRoot) {
  const ports = readdirSync(join(root, 'devices', 'ports'))
    .filter((f) => f.endsWith('.toml'))
    .sort()
    .map((f) => readFileSync(join(root, 'devices', 'ports', f), 'utf8'));
  return { ports, fw: fwTexts(root) };
}

const SOURCE_MAX = 256 * 1024;

/** The files of a module that the site shows: module.toml, README.md and src/** (text only), relative to the module, sorted. */
function sourceFiles(dir) {
  const out = ['module.toml', 'README.md'].filter((f) => existsSync(join(dir, f)));
  out.push(
    ...readdirSync(dir, { withFileTypes: true })
      .filter((e) => e.isFile() && e.name.startsWith('LICENSE'))
      .map((e) => e.name)
      .sort(),
  );
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

/** `patchkit pack-all` of the ids for one device: `{id: bytes}` of the packed ones; throws when any fails. */
function packAll(root, ids, device, to) {
  mkdirSync(to, { recursive: true });
  let stdout;
  let failure = '';
  try {
    stdout = patchkit(['pack-all', '--device', device, '--out', to, ...ids], { root });
  } catch (e) {
    stdout = e.stdout ?? '';
    failure = e.message;
  }
  const packed = {};
  const failed = [];
  for (const line of stdout.split('\n').filter((l) => l.trim().startsWith('{'))) {
    const r = JSON.parse(line);
    if (r.error !== undefined) failed.push(`${r.id}: ${r.error}`);
    else if (r.file !== undefined) packed[r.id] = readFileSync(resolve(root, r.file));
  }
  if (failed.length) throw new Error(`patchkit pack failed for ${failed.join('; ')} on ${device}`);
  if (failure) throw new Error(`patchkit pack failed on ${device}: ${failure}`);
  return packed;
}

/** Write the shipped modules under `out`: <dev>/<leaf>.dkm, index.json and src/<id>/**. Returns the index. */
export function shipModules(out, root = repoRoot) {
  const scratch = join(root, 'out', 'cache', 'site-pack');
  for (const sub of ['dt2', 'dn2', 'src']) rmSync(join(out, sub), { recursive: true, force: true });
  const modules = [];
  const manifests = ['machines', 'fx', 'tools'].flatMap((r) =>
    walk(join(root, 'modules', r), 'module.toml').sort(byParts),
  );
  const shipped = [];
  const idsByDevice = {};
  for (const file of manifests) {
    const dir = dirname(file);
    const d = parse(readFileSync(file, 'utf8'));
    if (!isShipped(d, relative(root, dir).split(sep).join('/'))) continue;
    shipped.push({ dir, d });
    for (const dev of shippedDevices(d)) (idsByDevice[dev] ??= []).push(d.module.id);
  }
  const packed = {};
  for (const [dev, ids] of Object.entries(idsByDevice)) packed[dev] = packAll(root, ids, dev, join(scratch, dev));
  for (const { dir, d } of shipped) {
    const m = d.module;
    const leaf = m.id.split('/').pop();
    const devices = {};
    for (const dev of shippedDevices(d)) {
      const bytes = packed[dev][m.id];
      if (!bytes) continue; // skipped by the packer (patch-only)
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
  // the unbound .dks services (dist/modules/<dev>/*.dks): the Transfer page binds them to the user's firmware
  const services = {};
  const servicesRoot = join(root, 'dist', 'modules');
  if (existsSync(servicesRoot)) {
    for (const dev of readdirSync(servicesRoot, { withFileTypes: true }).filter((e) => e.isDirectory())) {
      const names = readdirSync(join(servicesRoot, dev.name))
        .filter((f) => f.endsWith('.dks'))
        .sort();
      for (const name of names) {
        const bytes = readFileSync(join(servicesRoot, dev.name, name));
        mkdirSync(join(out, dev.name), { recursive: true });
        writeFileSync(join(out, dev.name, name), bytes);
        (services[dev.name] ??= []).push({ file: `${dev.name}/${name}`, size: bytes.length, sha256: sha256(bytes) });
      }
    }
  }
  const index = { modules, services };
  writeFileSync(join(out, 'index.json'), JSON.stringify(index, null, 1));
  return index;
}

const MODULE_FILES_TS = join(
  here,
  '..',
  '..',
  '..',
  'packages',
  'workbench-core',
  'src',
  'transfer',
  'module-files.generated.ts',
);

/**
 * The text of packages/workbench-core/src/transfer/module-files.generated.ts: module id to the `[files]` table of its module.toml (the files it owns under
 * `data/<module id>/`). Committed, because the desktop build does not run this script; `--check` fails when the file is stale.
 */
export function moduleFilesSource(root = repoRoot) {
  const entries = [];
  const manifests = ['machines', 'fx', 'tools'].flatMap((r) =>
    walk(join(root, 'modules', r), 'module.toml').sort(byParts),
  );
  for (const file of manifests) {
    const d = parse(readFileSync(file, 'utf8'));
    if (!d.files) continue;
    const spec = { label: d.files.label, ext: d.files.ext, preview: d.files.preview };
    entries.push(`  ${JSON.stringify(d.module.id)}: ${JSON.stringify(spec)},`);
  }
  return [
    '// Generated by apps/web/scripts/gen-modules.mjs from the [files] tables of module.toml. Do not edit.',
    "import type { ModuleFilesSpec } from './env';",
    '',
    'export const MODULE_FILES: Record<string, ModuleFilesSpec> = {',
    ...entries,
    '};',
    '',
  ].join('\n');
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url) &&
  process.argv.includes('--check')
) {
  const stale = !existsSync(MODULE_FILES_TS) || readFileSync(MODULE_FILES_TS, 'utf8') !== moduleFilesSource();
  if (stale) {
    console.error(
      'packages/workbench-core/src/transfer/module-files.generated.ts is stale: run node apps/web/scripts/gen-modules.mjs',
    );
    process.exit(1);
  }
  console.log('module-files.generated.ts is current');
} else if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  writeFileSync(MODULE_FILES_TS, moduleFilesSource());
  const i = process.argv.indexOf('--out');
  const out = i > 0 ? resolve(process.argv[i + 1]) : join(here, '..', 'public', 'modules');
  const { bundle } = buildModuleData();
  mkdirSync(out, { recursive: true });
  writeFileSync(join(out, 'bundle.json'), JSON.stringify(bundle));
  writeFileSync(join(out, 'bind.json'), JSON.stringify(buildBindBundle()));
  const index = shipModules(out);
  console.log(`shipped: ${index.modules.map((m) => `${m.id} (${Object.keys(m.devices).join(',')})`).join(', ')}`);
  console.log(
    `modules: ${bundle.modules.length} modules, ${bundle.ports.length} ports, bundle ${statSync(join(out, 'bundle.json')).size} bytes`,
  );
}
