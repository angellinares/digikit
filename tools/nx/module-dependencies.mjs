// Module graph edges from manifests, source ownership and acceptance metadata.
import { join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { read, records } from './module-manifests.mjs';

export const createDependencies = (_opts, ctx) => {
  const mods = Object.values(ctx.projects).filter((p) => p.tags?.includes('module'));
  const manifests = new Map(mods.map((p) => [p.name, read(ctx.workspaceRoot, join(p.root, 'module.toml'))]));
  const runtimes = mods.filter((p) => manifests.get(p.name).module.role === 'runtime');
  if (runtimes.length !== 1) throw new Error(`Expected one module with role=runtime, found ${runtimes.length}`);
  const runtime = runtimes[0].name;
  const owner = (path) =>
    mods
      .filter((p) => path === p.root || path.startsWith(p.root + '/'))
      .sort((a, b) => b.root.length - a.root.length)[0]?.name;
  // Shipped content modules (not core/*, not fixtures) need the runtime and the adapters of their type.
  const implicit = (p, type) =>
    !p.root.startsWith('modules/') || p.root.startsWith('modules/core/')
      ? []
      : [
          runtime,
          ...(type === 'machine' ? ['core/dynreg', 'core/dsp-voice'] : []),
          ...(type === 'fx' ? ['core/dynfx', 'core/fxpages'] : []),
        ];
  const all = Object.values(ctx.projects);
  return all
    .filter((p) => p.tags?.includes('module') || p.tags?.includes('test-only'))
    .flatMap((p) => {
      const isModule = p.tags?.includes('module');
      const t = isModule ? manifests.get(p.name) : { module: {} };
      const deps = isModule ? [...(t.module.requires ?? []), ...implicit(p, t.module.type)] : [];
      for (const o of t.object ?? [])
        for (const s of o.sources ?? []) deps.push(owner(normalize(join(p.root, s))) ?? '');
      const statics = [...new Set(deps)]
        .filter((d) => d && d !== p.name)
        .map((target) => ({
          source: p.name,
          target,
          sourceFile: join(p.root, 'module.toml'),
          type: DependencyType.static,
        }));
      // Implicit edges: a module with device tests depends on the harness, the daemon and the packs;
      // only a module with test-conformance also depends on the conformance suite.
      const hasDeviceTests = !!(
        p.targets?.['test-device']?.options?.command ||
        p.targets?.['test-device-full']?.options?.command ||
        p.targets?.['test-conformance']
      );
      const implicitDeps = hasDeviceTests ? ['regression', 'digi-control', 'patchkit', 'devices', 'sdk'] : [];
      // Runtime acceptance records name the fixture/content modules loaded by device tests.
      // Their source changes must invalidate the consumer's device-test cache.
      for (const rec of records(join(ctx.workspaceRoot, p.root))) {
        implicitDeps.push(...(rec.modules ?? []), ...Object.values(rec.aliases ?? {}));
      }
      if (p.targets?.['test-conformance']) implicitDeps.push('conformance');
      // Projects that live inside the module directory (for example core/ui and dkui-host).
      for (const q of all)
        if (q.name !== p.name && q.root.startsWith(p.root + '/') && !q.tags?.includes('test-only'))
          implicitDeps.push(q.name);
      const implicits = [...new Set(implicitDeps)]
        .filter((d) => d !== p.name && ctx.projects[d])
        .map((target) => ({ source: p.name, target, type: DependencyType.implicit }));
      return [...statics, ...implicits];
    });
};
