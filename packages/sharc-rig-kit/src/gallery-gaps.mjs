// The gallery rule of a machine module, in plain JS so the rig kit and the Nx plugin (tools/nx) share it.
// Problems of the module's `build/gen/gallery.json` (GALLERY, parsed; FILE names it in the messages) as `device: text` lines.
// A device of `[targets]` or `ports` of module.toml (or of an object's `devices`) that a machine of the gallery does not list,
// or lists with no parameter (unless `[uses.DEVICE] param_rows = 0` declares none), has no knob slots there: the rig would run
// it with every parameter word at 0. A module without `[[machine]]` has no gaps.
export const hasGallery = (manifest) => !!manifest.machine?.length;

export function galleryGaps(manifest, gallery, file) {
  if (!hasGallery(manifest)) return [];
  const fromPort = (t) => t.split('-')[0];
  const wanted = new Set([
    ...Object.keys(manifest.targets ?? {}).map(fromPort),
    ...(manifest.module?.ports ?? []).map(fromPort),
    ...(manifest.object ?? []).flatMap((o) => o.devices ?? []),
  ]);
  const gaps = [];
  for (const m of gallery.machines ?? [])
    for (const d of [...wanted].sort()) {
      if (!m.devices?.[d]) gaps.push(`${d}: machine ${m.id} is not listed in ${file}`);
      else if (!(gallery.params?.[d] ?? []).length && manifest.uses?.[d]?.param_rows !== 0)
        gaps.push(`${d}: machine ${m.id} has no parameter in ${file}`);
    }
  return gaps;
}
