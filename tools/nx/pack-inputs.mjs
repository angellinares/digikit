// Pack reads its explicit manifest library and builds the selected objects, not acceptance graph edges.
import { existsSync, readFileSync } from 'node:fs';
import { dirname, isAbsolute, join, normalize } from 'node:path';
import { pathDeps } from './cargo.mjs';
import { read } from './module-manifests.mjs';

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
  for (const [id, module] of byId) {
    if (!manifests.includes(module.file)) continue;
    projects.add(id);
    for (const object of module.objects)
      for (const source of object.sources ?? []) {
        if (isAbsolute(source) || /[?*[]/.test(source)) throw new Error(`Ambiguous pack source path: ${source}`);
        const path = bounded(join(dirname(module.file), source));
        include(path);
        if (path.endsWith('/Cargo.toml')) includeCrate(dirname(path));
      }
  }
  return [{ input: 'production', projects: [...projects].sort() }, ...[...paths].sort()];
};
