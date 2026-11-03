// Device suites share the regression aliases/TypeScript options. Follow their local source imports
// instead of making every runtime helper an input of every device task. Package/toolchain inputs
// remain declared by the caller; node_modules are deliberately outside this source closure.
import { existsSync, readFileSync } from 'node:fs';
import { createRequire, isBuiltin } from 'node:module';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';

export const testImportInputs = (workspaceRoot) => {
  const config = join(workspaceRoot, 'tests/regression/tsconfig.json');
  const ts = createRequire(join(workspaceRoot, 'tests/regression/package.json'))('typescript');
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
    const check = (node) => {
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
    for (const { fileName: specifier } of ts.preProcessFile(source, true, true).importedFiles) {
      const resolved = ts.resolveModuleName(specifier, file, options, ts.sys).resolvedModule?.resolvedFileName;
      if (resolved) {
        if (local(resolved)) result.push(resolved);
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
      result.push(missing);
    }
    return result;
  };
  return (files) => {
    const visited = new Set();
    const visit = (file) => {
      if (visited.has(file)) return;
      visited.add(file);
      for (const imported of imports(file)) visit(imported);
    };
    for (const file of files) visit(resolve(workspaceRoot, file));
    return [...visited].map((file) => `{workspaceRoot}/${relative(workspaceRoot, file)}`).sort();
  };
};
