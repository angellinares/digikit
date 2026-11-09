// Which modules the site ships as prebuilt .dkm files (the Transfer page's library): the run-time content modules of modules/machines,
// modules/fx and modules/tools. The rule of tools/device/dkpackall.py (`candidates`) without the core, framework and test modules.
// Written without TS-only syntax: scripts/gen-modules.mjs (node) and data/modules.ts (vite) both import it.

const ROOTS = ['modules/machines/', 'modules/fx/', 'modules/tools/'];

/** `manifest` is the parsed module.toml; `dir` the module's directory relative to the repository root, with `/`. */
export function isShipped(manifest, dir) {
  const m = manifest.module ?? {};
  if (!ROOTS.some((r) => `${dir}/`.startsWith(r))) return false;
  if (m.category === 'core' || m.type === 'framework') return false;
  // A developer tool sets `site = false`: the public site is for end users.
  if (m.site === false) return false;
  return Boolean(m.dynamic) || m.type === 'raw' || m.type === 'service';
}

/** The device ids ('dt2', 'dn2') that the manifest does not mark unsupported (a pack that fails still leaves a device out). */
export function shippedDevices(manifest) {
  const out = new Set();
  for (const [port, status] of Object.entries(manifest.targets ?? {}))
    if (status !== 'unsupported') out.add(port.split('-')[0]);
  return ['dt2', 'dn2'].filter((d) => out.has(d));
}
