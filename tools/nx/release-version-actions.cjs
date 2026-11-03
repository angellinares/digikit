// Nx owns version selection and dry-run handling; this adapter only edits our source manifests.
const { VersionActions } = require('nx/release');
const { join } = require('node:path');
const { parse } = require('smol-toml');

function validateVersion(version) {
  if (
    typeof version !== 'string' ||
    !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version) ||
    version.split('.').some((part) => Number(part) > 255)
  ) {
    throw new Error(
      `Unsupported packed version ${JSON.stringify(version)}: use major.minor.patch with parts 0..255, without prerelease or build metadata`,
    );
  }
}

function snapshot(tree, path, table) {
  const text = tree.read(path, 'utf8');
  if (text === null) throw new Error(`Missing version manifest ${path}`);
  const version = parse(text)[table]?.version;
  validateVersion(version);
  // Edit one scalar, never serialize the TOML: comments, quoting and line endings stay intact.
  let active = false;
  let offset = 0;
  const matches = [];
  for (const line of text.match(/[^\n]*\n|[^\n]+$/g) ?? []) {
    if (/^[ \t]*\[/.test(line))
      active = new RegExp(`^[ \\t]*\\[${table}\\][ \\t]*(?:#[^\\r\\n]*)?\\r?\\n?$`).test(line);
    if (active) {
      const match = /^([ \t]*version[ \t]*=[ \t]*)(["'])([^"'\r\n]*)\2[ \t]*(?:#[^\r\n]*)?\r?\n?$/.exec(line);
      if (match)
        matches.push({ start: offset + match[1].length + 1, end: offset + match[1].length + 1 + match[3].length });
    }
    offset += line.length;
  }
  if (matches.length !== 1 || text.slice(matches[0].start, matches[0].end) !== version) {
    throw new Error(`${path}: expected one single-line quoted [${table}].version; refusing to reformat TOML`);
  }
  return { path, text, version, ...matches[0] };
}

class TomlVersionActions extends VersionActions {
  constructor(releaseGroup, projectGraphNode, finalConfigForProject) {
    super(releaseGroup, projectGraphNode, finalConfigForProject);
    const options = finalConfigForProject.versionActionsOptions ?? {};
    const filename = options.manifestFilename ?? 'module.toml';
    this.versionTable = options.versionTable ?? 'module';
    this.mirrorProjectManifest = options.mirrorProjectManifest === true;
    if (!(
      (filename === 'module.toml' && this.versionTable === 'module' && !this.mirrorProjectManifest) ||
      (filename === 'sdk.toml' && this.versionTable === 'runtime' && this.mirrorProjectManifest)
    )) {
      throw new Error('Version actions support module.toml [module], or sdk.toml [runtime] with a module mirror');
    }
    this.validManifestFilenames = [filename];
    this.excludeManifestsFromFormatting = true;
  }

  async init(tree) {
    await super.init(tree);
    if (this.manifestsToUpdate.length !== 1) throw new Error('Expected exactly one canonical version manifest');
    this.manifestSpecs = [{ path: this.manifestsToUpdate[0].manifestPath, table: this.versionTable }];
    if (this.mirrorProjectManifest) {
      const path = join(this.projectGraphNode.data.root, 'module.toml');
      this.manifestSpecs.push({ path, table: 'module' });
      this.manifestsToUpdate.push({ manifestPath: path, preserveLocalDependencyProtocols: false });
    }
  }

  snapshots(tree) {
    const manifests = this.manifestSpecs.map(({ path, table }) => snapshot(tree, path, table));
    if (manifests.some((m) => m.version !== manifests[0].version)) {
      throw new Error(`Runtime version mirror mismatch: ${manifests.map((m) => `${m.path}=${m.version}`).join(', ')}`);
    }
    return manifests;
  }

  async validate(tree) {
    await super.validate(tree);
    this.snapshots(tree);
  }

  async calculateNewVersion(...args) {
    if (/^\d/.test(args[1])) validateVersion(args[1]);
    const result = await super.calculateNewVersion(...args);
    validateVersion(result.newVersion);
    return result;
  }

  async readCurrentVersionFromSourceManifest(tree) {
    const manifest = this.snapshots(tree)[0];
    return { currentVersion: manifest.version, manifestPath: manifest.path };
  }

  async readCurrentVersionFromRegistry() {
    throw new Error('TOML releases require currentVersionResolver: disk; no registry is used');
  }

  async readDependencies() {
    // Image requires/build/test edges are not versioned manifest dependency declarations.
    // Runtime ranges and loading enforcement are explicitly deferred.
    return [];
  }

  async readCurrentVersionOfDependency() {
    return { currentVersion: null, dependencyCollection: null };
  }

  async updateProjectVersion(tree, newVersion) {
    validateVersion(newVersion);
    const manifests = this.snapshots(tree); // validate both before writing either
    for (const m of manifests) tree.write(m.path, m.text.slice(0, m.start) + newVersion + m.text.slice(m.end));
    return manifests.map((m) => `${m.path}: ${m.version} -> ${newVersion}`);
  }

  async updateProjectDependencies(_tree, _graph, dependenciesToUpdate) {
    if (Object.keys(dependenciesToUpdate).length) throw new Error('Versioned manifest dependencies are not configured');
    return [];
  }
}

module.exports = TomlVersionActions;
