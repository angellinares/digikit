// Pack reads its explicit manifest library and builds the selected objects, not acceptance graph edges.
import { existsSync, globSync, readFileSync, statSync } from 'node:fs';
import { dirname, isAbsolute, join, normalize } from 'node:path';
import { pathDeps } from './cargo.mjs';
import { read } from './module-manifests.mjs';

// Headers of sdk/include that a build reads. A textual scan of `#include` and `.include` over the build's sources and the
// headers they reach (conditional includes count: the scan is wider than the preprocessor). A name is looked up like the
// patchkit build does: beside the including file, in the modules' own include, src and build/gen directories, then the pinned
// include directories (tools/patchkit/toolchain/pins.toml). A name that no directory holds is not an input.
// Limit: a computed include (`#include MACRO`) is not seen.
const INCLUDE = /^[ \t]*(?:#[ \t]*include|\.include)[ \t]*[<"]([^>"\n]+)[>"]/gm;
const SDK_INCLUDE = 'sdk/include/';
const sdkHeaders = (workspaceRoot, sources, moduleDirs) => {
  const pins = read(workspaceRoot, 'tools/patchkit/toolchain/pins.toml').include;
  const dirs = [
    ...moduleDirs.flatMap((dir) => [`${dir}/build/gen`, `${dir}/src`, `${dir}/include`, dir]),
    ...new Set([...pins.cf, ...pins.sharc]),
  ];
  const seen = new Set();
  const found = new Set();
  const visit = (file) => {
    if (seen.has(file)) return;
    seen.add(file);
    const path = join(workspaceRoot, file);
    if (!existsSync(path) || !statSync(path).isFile()) return;
    for (const [, name] of readFileSync(path, 'utf8').matchAll(INCLUDE)) {
      for (const dir of [dirname(file), ...dirs]) {
        const candidate = normalize(join(dir, name));
        if (existsSync(join(workspaceRoot, candidate)) && statSync(join(workspaceRoot, candidate)).isFile()) {
          if (candidate.startsWith(SDK_INCLUDE)) found.add(candidate);
          visit(candidate);
          break;
        }
      }
    }
  };
  for (const source of sources) visit(source);
  return [...found].sort().map((file) => `{workspaceRoot}/${file}`);
};

export const packSourceInputs = (workspaceRoot, byId, manifests) => {
  const bounded = (path) => {
    const normalized = normalize(path);
    if (isAbsolute(normalized) || normalized === '..' || normalized.startsWith('../'))
      throw new Error(`Pack source is outside the workspace: ${path}`);
    return normalized;
  };
  const owners = [...byId].map(([name, module]) => ({ name, root: dirname(module.file) }));
  const cargo = join(workspaceRoot, 'Cargo.toml');
  if (existsSync(cargo))
    for (const root of read(workspaceRoot, 'Cargo.toml').workspace?.members ?? []) {
      if (isAbsolute(root) || bounded(root) !== root || /[?*[]/.test(root))
        throw new Error(`Ambiguous Cargo workspace member ${root}`);
      owners.push({ name: read(workspaceRoot, join(root, 'Cargo.toml')).package.name, root });
    }
  const projects = new Set();
  const paths = new Set();
  const include = (path, directory = false) => {
    path = bounded(path);
    // Cargo directories retain project ownership, including non-workspace path crates.
    if (directory)
      for (let root = path; root !== '.'; root = dirname(root)) {
        const file = join(workspaceRoot, root, 'project.json');
        if (existsSync(file)) {
          const project = JSON.parse(readFileSync(file, 'utf8'));
          if (project.name) owners.push({ name: project.name, root });
          break;
        }
      }
    const candidates = owners.filter((owner) => path === owner.root || path.startsWith(`${owner.root}/`));
    candidates.sort((a, b) => b.root.length - a.root.length);
    const owner = candidates[0];
    if (owner && candidates.some((other) => other.root === owner.root && other.name !== owner.name))
      throw new Error(`Ambiguous pack source owner: ${path}`);
    if (owner) projects.add(owner.name);
    else {
      paths.add(`{workspaceRoot}/${path}${directory ? '/**/*' : ''}`);
      // External C/C++ objects read private headers beside their explicit sources. Public
      // include roots are shared build inputs; a project owner must not widen these sources.
      if (!directory && /\.(?:c|cc|cpp|cxx)$/.test(path))
        paths.add(`{workspaceRoot}/${dirname(path)}/**/*.{h,hh,hpp,hxx}`);
    }
  };
  const crates = new Set();
  const includeCrate = (root) => {
    root = bounded(root);
    if (crates.has(root)) return;
    crates.add(root);
    include(root, true);
    const doc = read(workspaceRoot, join(root, 'Cargo.toml'));
    for (const table of [doc, ...Object.values(doc.target ?? {})])
      for (const kind of ['dependencies', 'dev-dependencies', 'build-dependencies'])
        for (const dependency of Object.values(table[kind] ?? {})) {
          if (dependency?.workspace) throw new Error(`Pack Cargo dependency inherits workspace configuration: ${root}`);
          if (dependency?.path && isAbsolute(dependency.path))
            throw new Error(`Pack Cargo dependency is outside the workspace: ${dependency.path}`);
        }
    for (const dependency of pathDeps(workspaceRoot, root)) includeCrate(dependency);
  };
  const buildSources = new Set();
  const buildDirs = new Set();
  for (const [id, module] of byId) {
    if (!manifests.includes(module.file)) continue;
    projects.add(id);
    buildDirs.add(dirname(module.file));
    for (const object of module.objects)
      for (const source of object.sources ?? []) {
        if (isAbsolute(source) || /[?*[]/.test(source)) throw new Error(`Ambiguous pack source path: ${source}`);
        const path = bounded(join(dirname(module.file), source));
        include(path);
        buildSources.add(path);
        if (path.endsWith('/Cargo.toml')) includeCrate(dirname(path));
        // a content module's C object links the SDK components it uses (patchkit build, sdk_lib): all of sdk/src is an input
        if (path.endsWith('.c') && module.manifest?.category !== 'core') {
          paths.add('{workspaceRoot}/sdk/src/**/*');
          for (const file of globSync('sdk/src/**/*', { cwd: workspaceRoot })) buildSources.add(file);
        }
      }
  }
  return [
    { input: 'production', projects: [...projects].sort() },
    ...[...paths].sort(),
    ...sdkHeaders(workspaceRoot, buildSources, [...buildDirs].sort()),
  ];
};
