// Checks the cargo dep-info files of the cached rust-tests targets (HERMETIC in tools/nx/cargo.mjs) against their
// declared inputs. Every repository file that a test unit compiles in must be under its crate, under a crate it
// depends on by path (transitively), in the extra inputs of one of those crates, or the root Cargo.toml/Cargo.lock.
// Run after the targets ran: `node tools/nx/cargo-depfiles.mjs`. Exit 1 on uncovered files.
// Run-time reads (std::fs) are not in dep-info: HERMETIC crates must have none.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';
import { HERMETIC, pathDeps } from './cargo.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const WS = '{workspaceRoot}/';

const members = parse(readFileSync(join(root, 'Cargo.toml'), 'utf8')).workspace?.members ?? [];
const byName = new Map();
const rootToName = new Map();
for (const r of members) {
  const { name } = parse(readFileSync(join(root, r, 'Cargo.toml'), 'utf8')).package;
  byName.set(name, r);
  rootToName.set(r, name);
}
const roots = [...byName.values()];

const depsDir = join(root, 'dist/cargo-tests/release/deps');
if (!existsSync(depsDir)) {
  console.error('cargo-depfiles: dist/cargo-tests/release/deps missing: run the hermetic rust-tests targets first');
  process.exit(2);
}

const under = (path, r) => path === r || path.startsWith(r + '/');
const closure = (name) => {
  const seen = new Set();
  const visit = (n) => {
    if (seen.has(n) || !byName.has(n)) return;
    seen.add(n);
    for (const d of pathDeps(root, byName.get(n))) if (rootToName.has(d)) visit(rootToName.get(d));
  };
  visit(name);
  return [...seen];
};

const parseDepFile = (text) => {
  const deps = [];
  for (const line of text.replace(/\\\r?\n/g, ' ').split('\n')) {
    let rest;
    const i = line.indexOf(': ');
    if (i >= 0) rest = line.slice(i + 2);
    else if (line.endsWith(':')) rest = '';
    else continue;
    for (const part of rest.split(/(?<!\\) +/)) {
      const p = part.replaceAll('\\ ', ' ');
      if (p && !deps.includes(p)) deps.push(p);
    }
  }
  return deps;
};

const rel = (p) => relative(root, resolve(root, p));
let units = 0;
const owners = new Set();
const bad = [];
for (const file of readdirSync(depsDir).filter((f) => f.endsWith('.d'))) {
  const deps = parseDepFile(readFileSync(join(depsDir, file), 'utf8'));
  if (!deps.length) continue;
  const first = rel(deps[0]);
  if (first.startsWith('..')) continue;
  const owner = roots.filter((r) => under(first, r)).sort((a, b) => b.length - a.length)[0];
  const name = owner && rootToName.get(owner);
  if (!name || !(name in HERMETIC)) continue;
  units++;
  owners.add(name);
  const crates = closure(name);
  const extras = new Set(crates.flatMap((c) => HERMETIC[c] ?? []).map((e) => e.replace(WS, '')));
  for (const dep of deps) {
    const path = rel(dep);
    if (path.startsWith('..') || under(path, 'dist')) continue;
    const ok =
      crates.some((c) => under(path, byName.get(c))) ||
      path === 'Cargo.toml' ||
      path === 'Cargo.lock' ||
      extras.has(path);
    if (!ok) bad.push(`${name}: ${path} (${file})`);
  }
}
if (bad.length) {
  console.error(bad.join('\n'));
  process.exit(1);
}
console.log(`cargo-depfiles: ${units} units of ${owners.size} crates covered`);
