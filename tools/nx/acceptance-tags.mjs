// The closed vocabulary of `tags` on `metadata.runtimeAcceptance` records, and the check that every device test file has tags.
// Tags say which service or layer a device test proves (docs/plans/test-architecture.md, T1e).
//   S:<service>   machines, samples, lfo (a service in the runtime image; fx is a service file, `fx.dks`: its tests carry the tags of the adapters they prove)
//   C:<area>      state, boot, loading, dsp, storage, transport, diagnostics, tick, layout
//   A:<id>        an adapter or app module of category core (the module must exist)
//   M:<id>        a tool module (the module must exist under modules/tools)
// `C:layout` marks tests that depend on addresses: addrmap, log-names, memaudit, crash and fault tests.
export const SERVICES = ['machines', 'samples', 'lfo'];
export const AREAS = ['state', 'boot', 'loading', 'dsp', 'storage', 'transport', 'diagnostics', 'tick', 'layout'];

// Why `tag` is not in the vocabulary, or '' when it is. `modules`: id -> { category, root } of every module.
export const tagProblem = (tag, modules) => {
  if (typeof tag !== 'string') return 'is not a string';
  const [kind, name] = [tag.slice(0, 2), tag.slice(2)];
  if (kind === 'S:') return SERVICES.includes(name) ? '' : `names no service (${SERVICES.join(', ')})`;
  if (kind === 'C:') return AREAS.includes(name) ? '' : `names no area (${AREAS.join(', ')})`;
  if (kind === 'A:' || kind === 'M:') {
    const module = modules.get(name);
    if (!module) return 'names no module';
    if (kind === 'A:' && module.category !== 'core') return 'names a module that is not of category core';
    if (kind === 'M:' && !module.root?.startsWith('modules/tools/'))
      return 'names a module that is not under modules/tools';
    return '';
  }
  return 'has no S:, C:, A: or M: prefix';
};

// Throws when a record has unknown tags, or (with `deviceTests`) when a device test file is in no tagged record.
// `recs`: the runtimeAcceptance records of project `root`; `deviceTests`: its device test files relative to the root.
export const checkAcceptanceTags = (root, recs, deviceTests, modules) => {
  for (const rec of recs) {
    if (rec.tags !== undefined && !Array.isArray(rec.tags))
      throw new Error(`metadata.runtimeAcceptance tags of ${root} must be an array`);
    for (const tag of rec.tags ?? []) {
      const problem = tagProblem(tag, modules);
      if (problem) throw new Error(`metadata.runtimeAcceptance of ${root}: tag ${JSON.stringify(tag)} ${problem}`);
    }
  }
  const tagged = new Set(recs.filter((rec) => rec.tags?.length).flatMap((rec) => rec.files ?? []));
  const untagged = deviceTests.filter((t) => !tagged.has(t));
  if (untagged.length)
    throw new Error(`metadata.runtimeAcceptance of ${root} has no tags for the device tests: ${untagged.join(', ')}`);
};
