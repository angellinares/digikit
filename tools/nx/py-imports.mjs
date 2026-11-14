// Python inputs of a device test. Given module names such as `tools.device.dkproto` (found in the test's TypeScript
// import closure), follow the `tools.*` imports of each Python file and return the files as exact Nx input paths.
// An edge is found by regex over the whole file text, so imports inside functions count: `from tools.a import b` (and
// `tools.a.b` when that is a module), `import tools.a`, and any string literal that holds a `tools.x.y` name (`-m tools.x`
// spawns, importlib with a literal name).
// Lock-only modules (NO_RESULT_MODULES) are skipped: `tools.devtools.benchlock` only serialises runs and changes no result,
// so an edit of it must not reselect the device suite. The gate roots (tools/gate/*/project.json) list their Python by hand
// and leave it out too.
// Limits: relative imports (`from .`, used only in tools/sharc_dev) are not followed; computed module names are not seen;
// only files that exist are returned; data files and non-Python tools a script runs are not inputs.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const FROM_IMPORT = /^\s*from\s+(tools(?:\.\w+)+)\s+import\s+([\w, ()]+)/gm;
const IMPORT = /^\s*import\s+(tools(?:\.\w+)+)/gm;
const LITERAL = /['"](tools(?:\.\w+)+)['"]/g;
const NO_RESULT_MODULES = new Set(['tools.devtools.benchlock']);

export const pythonImportInputs = (workspaceRoot) => {
  const cache = new Map();
  // module name -> the files it loads: the module file or package, plus the `__init__.py` of each package on its path
  const filesOf = (name) => {
    if (cache.has(name)) return cache.get(name);
    const parts = name.split('.');
    const files = [];
    for (let i = 1; i < parts.length; i++) {
      const init = `${parts.slice(0, i).join('/')}/__init__.py`;
      if (existsSync(join(workspaceRoot, init))) files.push(init);
    }
    const base = parts.join('/');
    if (existsSync(join(workspaceRoot, `${base}.py`))) files.push(`${base}.py`);
    else if (existsSync(join(workspaceRoot, base, '__init__.py'))) files.push(`${base}/__init__.py`);
    cache.set(name, files);
    return files;
  };
  const isModule = (name) => {
    const base = name.split('.').join('/');
    return existsSync(join(workspaceRoot, `${base}.py`)) || existsSync(join(workspaceRoot, base, '__init__.py'));
  };
  const edgesCache = new Map();
  const edges = (file) => {
    if (edgesCache.has(file)) return edgesCache.get(file);
    const found = new Set();
    const text = readFileSync(join(workspaceRoot, file), 'utf8');
    for (const [, module, names] of text.matchAll(FROM_IMPORT)) {
      found.add(module);
      for (const name of names.split(/[\s,()]+/).filter((n) => /^\w+$/.test(n)))
        if (isModule(`${module}.${name}`)) found.add(`${module}.${name}`);
    }
    for (const [, module] of text.matchAll(IMPORT)) found.add(module);
    for (const [, module] of text.matchAll(LITERAL)) found.add(module);
    const result = [...found];
    edgesCache.set(file, result);
    return result;
  };
  return (seedModules) => {
    const seen = new Set();
    const files = new Set();
    const visit = (name) => {
      if (seen.has(name) || NO_RESULT_MODULES.has(name)) return;
      seen.add(name);
      for (const file of filesOf(name)) {
        files.add(file);
        if (file.endsWith('.py')) for (const next of edges(file)) visit(next);
      }
    };
    if (seedModules.length) files.add('tools/__init__.py');
    for (const name of seedModules) visit(name);
    return [...files]
      .filter((file) => existsSync(join(workspaceRoot, file)))
      .sort()
      .map((file) => `{workspaceRoot}/${file}`);
  };
};
