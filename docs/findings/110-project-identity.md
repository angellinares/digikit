# 110. Project identity for digikit stores (DT2 1.17, DN2 1.12)

Trigger: digikit stores data per project and needs a stable project identity. Results are read once from the images. No device run. Marks: **[D]** measured or read once, **[O]** open.

## 1. The current project slot **[D]**

The current project slot (0..127, card slot `0x58000 + s*0x8000`, [finding 23](23-digikit-runtime-spikes.md)) is one global byte.

| Device | Byte | Setter | Notes |
| --- | --- | --- | --- |
| DT2 | `0x4099d5c5` (change signal `0x4099d5c4`) | `0x400c11ca(slot)` | writes only on change, notifies via `0x400c102c` |
| DN2 | `0x405ce899` | `0x400bb7fa` | `0x4003eb62` pairs `0x400424a2` |

On DT2 the setter is reached from `0x400424a2(Project*, slot)` (8 callers) and from `project_container_decode` `0x400c0d90`. On DN2, `0x400424a2` has 8 callers.

DT2 project paths:

| Function | Address |
| --- | --- |
| `Project::loadProjectFromMmc(slot)` | `0x40046906` (wrapper `0x40046c38`, from ProjectManager `0x400a227a`) |
| `Project::initializeNewProject(slot)` | `0x400465e2` |
| `Project::loadProjectFromMemory` | `0x40043f74` |

All three call `0x400424a2`, then `0x40042522(Project*, 0)`. The DN2 addresses of `loadProjectFromMmc` and `initializeNewProject` are **[O]**.

## 2. No stronger identity **[D]**

- There is no UUID or hash field. The header word at slot offset 0x18 is not an identity ([finding 39](39-digikit-boot-tests.md)).
- The slot byte alone is weak: delete, overwrite, save as, load from and save to can reuse a slot.
- A key of (device, slot, project name) is safer. The RAM name accessor is **[O]**.

## 3. What digikit exposes today **[D]**

digikit has no project identity and no project-change event. It has only the `devices/fw/project.toml` singleton, `active_kit`, `track_type` and `serialize`. `dkrt_track_type` returns -2 when there is no project.

`ReloadAllSamples` callers are not a project signal. Most are kit, boot or repair paths ([finding 109](109-dt2-stock-pool-borrowing.md)).
