// A barrel (a file of only `export ... from` statements, such as packages/dk-session/src/index.ts) is followed per name:
// `import { A } from '@digi/dk-session'` reaches the file that exports A, not every file the barrel re-exports. A namespace,
// default, side-effect, dynamic or `require` import of a file takes all of it. Limit: a re-exported file that a test loads
// through the barrel without using one of its names is not an input (its top-level code is not part of the key).
// Device suites share the regression aliases/TypeScript options. Follow their local source imports
// instead of making every runtime helper an input of every device task. Package/toolchain inputs
// remain declared by the caller; node_modules are deliberately outside this source closure.
import { existsSync, readFileSync } from 'node:fs';
import { createRequire, isBuiltin } from 'node:module';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// TypeScript is a dependency of tests/regression in the real checkout; a workspaceRoot that is a fixture has no node_modules.
const regressionRequire = createRequire(fileURLToPath(new URL('../../tests/regression/package.json', import.meta.url)));

export const testImportInputs = (workspaceRoot) => {
  const config = join(workspaceRoot, 'tests/regression/tsconfig.json');
  const ts = regressionRequire('typescript');
  const parsed = ts.getParsedCommandLineOfConfigFile(
    config,
    {},
    {
      ...ts.sys,
      onUnRecoverableConfigFileDiagnostic: (diagnostic) => {
        throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n'));
      },
    },
  );
  const errors = parsed.errors.filter((diagnostic) => diagnostic.code !== 18003); // Empty fixture projects are valid.
  if (errors.length)
    throw new Error(ts.formatDiagnosticsWithColorAndContext(errors, ts.createCompilerHost(parsed.options)));
  const options = parsed.options;
  const cache = new Map();
  const local = (file) => {
    const path = relative(workspaceRoot, file);
    return !isAbsolute(path) && !path.startsWith('../') && !path.split('/').includes('node_modules');
  };
  const imports = (file) => {
    if (cache.has(file)) return cache.get(file);
    // Keep an explicit missing .ts import in the inputs: a deleted helper must still select its consumers.
    const result = [];
    cache.set(file, result);
    if (!existsSync(file)) return result;
    const source = readFileSync(file, 'utf8');
    const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
    // Names each specifier asks for: a Set, or null when the whole file is used (sticky).
    const wanted = new Map();
    const want = (specifier, names) => {
      if (wanted.get(specifier) === null) return;
      if (!names) wanted.set(specifier, null);
      else wanted.set(specifier, new Set([...(wanted.get(specifier) ?? []), ...names]));
    };
    const check = (node) => {
      if (ts.isImportDeclaration(node) && ts.isStringLiteralLike(node.moduleSpecifier)) {
        const clause = node.importClause;
        const bindings = clause?.namedBindings;
        if (!clause || clause.name || !bindings || !ts.isNamedImports(bindings)) want(node.moduleSpecifier.text, null);
        else
          want(
            node.moduleSpecifier.text,
            bindings.elements.map((e) => (e.propertyName ?? e.name).text),
          );
      } else if (ts.isExportDeclaration(node) && node.moduleSpecifier) want(node.moduleSpecifier.text, null);
      else if (
        ts.isCallExpression(node) &&
        node.arguments.length === 1 &&
        ts.isStringLiteralLike(node.arguments[0]) &&
        (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
          (ts.isIdentifier(node.expression) && node.expression.text === 'require'))
      )
        want(node.arguments[0].text, null);
      if (
        ts.isCallExpression(node) &&
        (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
          (ts.isIdentifier(node.expression) && node.expression.text === 'require')) &&
        (node.arguments.length !== 1 || !ts.isStringLiteralLike(node.arguments[0]))
      )
        throw new Error(`Computed test import needs explicit input review: ${file}`);
      ts.forEachChild(node, check);
    };
    check(syntax);
    const names = (specifier) => wanted.get(specifier) ?? null;
    for (const { fileName: specifier } of ts.preProcessFile(source, true, true).importedFiles) {
      const resolved = ts.resolveModuleName(specifier, file, options, ts.sys).resolvedModule?.resolvedFileName;
      if (resolved) {
        if (local(resolved)) result.push({ path: resolved, names: names(specifier) });
        continue;
      }
      // Module resolution cannot resolve deleted files. Explicit source imports still have an
      // unambiguous path, including the paths aliases shared with Vitest in regression/tsconfig.json.
      let missing;
      if (specifier.startsWith('.')) missing = resolve(dirname(file), specifier);
      else
        for (const [pattern, replacements] of Object.entries(options.paths ?? {})) {
          const star = pattern.indexOf('*');
          const prefix = star < 0 ? pattern : pattern.slice(0, star);
          const suffix = star < 0 ? '' : pattern.slice(star + 1);
          if (star < 0 ? specifier !== pattern : !specifier.startsWith(prefix) || !specifier.endsWith(suffix)) continue;
          if (replacements.length !== 1) throw new Error(`Ambiguous local test import: ${specifier} in ${file}`);
          const matched = star < 0 ? '' : specifier.slice(prefix.length, specifier.length - suffix.length);
          missing = resolve(options.baseUrl ?? dirname(config), replacements[0].replace('*', matched));
          break;
        }
      if (!missing && isBuiltin(specifier)) continue;
      if (!missing) throw new Error(`Cannot resolve test import: ${specifier} in ${file}`);
      if (!/\.[cm]?tsx?$/.test(missing) || !local(missing))
        throw new Error(`Cannot resolve local test import: ${specifier} in ${file}`);
      result.push({ path: missing, names: null });
    }
    return result;
  };
  const parse = (file) => ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true);
  const resolveLocal = (specifier, file) => {
    const resolved = ts.resolveModuleName(specifier, file, options, ts.sys).resolvedModule?.resolvedFileName;
    return resolved && local(resolved) ? resolved : null;
  };
  // Barrel: only `export ... from` statements, every target a local file. Returns [{ path, star, ns, map }], or null.
  // map: exported name -> name in the target (named re-exports).
  const barrelCache = new Map();
  const barrel = (file) => {
    if (barrelCache.has(file)) return barrelCache.get(file);
    let result = null;
    if (existsSync(file)) {
      const { statements } = parse(file);
      const targets = [];
      const ok =
        statements.length > 0 &&
        statements.every((st) => {
          if (!ts.isExportDeclaration(st) || !st.moduleSpecifier) return false;
          const path = resolveLocal(st.moduleSpecifier.text, file);
          if (!path) return false;
          const clause = st.exportClause;
          if (!clause) targets.push({ path, star: true });
          else if (ts.isNamespaceExport(clause)) targets.push({ path, ns: clause.name.text });
          else
            targets.push({
              path,
              map: new Map(clause.elements.map((e) => [e.name.text, (e.propertyName ?? e.name).text])),
            });
          return true;
        });
      if (ok) result = targets;
    }
    barrelCache.set(file, result);
    return result;
  };
  // Names a file exports (through `export *` too). null when it cannot be told: take the whole file then.
  const exportsCache = new Map();
  const exportedNames = (file) => {
    if (exportsCache.has(file)) return exportsCache.get(file);
    exportsCache.set(file, new Set()); // cycle guard
    let result = new Set();
    if (!existsSync(file)) result = null;
    else
      for (const st of parse(file).statements) {
        const exported =
          ts.canHaveModifiers(st) && ts.getModifiers(st)?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword);
        if (ts.isExportAssignment(st)) result?.add('default');
        else if (ts.isExportDeclaration(st)) {
          const clause = st.exportClause;
          if (clause && ts.isNamedExports(clause)) for (const e of clause.elements) result?.add(e.name.text);
          else if (clause) result?.add(clause.name.text);
          else {
            const target = st.moduleSpecifier && resolveLocal(st.moduleSpecifier.text, file);
            const inner = target ? exportedNames(target) : null;
            if (!inner) result = null;
            else for (const n of inner) result?.add(n);
          }
        } else if (exported) {
          if (ts.isVariableStatement(st))
            for (const d of st.declarationList.declarations) {
              if (ts.isIdentifier(d.name)) result?.add(d.name.text);
              else result = null; // destructuring export: not worth parsing
            }
          else if (st.name) result?.add(st.name.text);
          else result = null;
          if (ts.getModifiers(st)?.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword)) result?.add('default');
        }
        if (result === null) break;
      }
    exportsCache.set(file, result);
    return result;
  };
  return (files) => {
    // file -> 'all' or the Set of names reached so far
    const reached = new Map();
    const visit = (file, names) => {
      const before = reached.get(file);
      if (before === 'all') return;
      let fresh;
      if (!names) {
        reached.set(file, 'all');
        fresh = null;
      } else {
        const merged = new Set(before ?? []);
        fresh = [...names].filter((n) => !merged.has(n));
        if (before && !fresh.length) return;
        for (const n of fresh) merged.add(n);
        reached.set(file, merged);
      }
      const targets = barrel(file);
      if (!targets) {
        if (!before || !fresh) for (const imported of imports(file)) visit(imported.path, imported.names);
        return;
      }
      // a barrel passes each wanted name on to the re-export that supplies it; an unknown name takes every target
      for (const target of targets) {
        if (!fresh) {
          visit(target.path, null);
          continue;
        }
        const taken = [];
        for (const name of fresh) {
          if (target.ns === name) taken.push(null);
          else if (target.map?.has(name)) taken.push(target.map.get(name));
          else if (target.star) {
            const known = exportedNames(target.path);
            if (!known || known.has(name)) taken.push(name);
          }
        }
        if (taken.includes(null)) visit(target.path, null);
        else if (taken.length) visit(target.path, taken);
      }
      // Names that no target supplies (a type from a path we cannot see): keep the barrel's files all
      for (const name of fresh ?? []) {
        const supplied = targets.some(
          (t) => t.ns === name || t.map?.has(name) || (t.star && (exportedNames(t.path)?.has(name) ?? true)),
        );
        if (!supplied) for (const t of targets) visit(t.path, null);
      }
    };
    for (const file of files) visit(resolve(workspaceRoot, file), null);
    return [...reached.keys()].map((file) => `{workspaceRoot}/${relative(workspaceRoot, file)}`).sort();
  };
};
