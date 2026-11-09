# digikit project

Status: plan, design points agreed with Em on 2026-11-02. Not started. Work happens on a new branch `digikit-project`, cut from main after the sampler branch (`sdk/dsp-sampler-discovery`) merges.

## Goal

A digikit project holds the digikit data that belongs to one Elektron project: the modules that project needs and each module's data for that project. Elektron project files stay stock. Stock save and load work as before, and a stock project without digikit data loads as before.

## Decisions

1. **One home for per-project data: a `.dkp` file in the digikit store**, one per Elektron project. Earlier docs named three homes (a project scope in the settings record, a `.dkp` in `~/src/digi/WORKBENCH-AND-MODULES-PLAN-2026-10-19.md`, and the "per-project selection store" in [sample-api-1.5](../design/sample-api-1.5.md)). The `.dkp` replaces all three.
2. **Module persistence API in the SDK, with two scopes.**
   - Module scope: the module's own settings, the same in every project. Stored in the settings record ([digikit-store](../design/digikit-store.md), "The settings record") under keys `<module id>/<key>`, the shape already reserved there and in [module-architecture](module-architecture.md) D19.
   - Project scope: the module's section of the current `.dkp`.
   - Draft calls: `dk_data_get/put/remove(scope, key, buf, len)`. The runtime adds the module id to each key, so a module cannot read another module's data.
   - Modules get a project-changed event through a named module export (like `dkm_menu`), called by the runtime. Slots cannot carry it: there are 8 single-holder slots and only slot 0 is called.
   - Project-scope entries use the settings-record entry format and its limits (key 1-47 printable bytes including the module id and `/`, value at most 1024 bytes). Larger data uses one key per item; the pool list uses `pool/0` .. `pool/15`.
3. **The runtime stores; modules decide what to store.**
   - The runtime keeps the shared sample pool mechanics: memory, loading, dedup, refcount, DT2 stock-slot borrowing. These are shared between modules and tied to DSP reserve, the DSP-link queue and the capacity checks ([module-architecture](module-architecture.md) section 3.6), and there is no module-to-module API.
   - The runtime drops its per-project pool lists: the `Proj` rows and `project_changed` handling in `modules/core/runtime/sdk/src/sample_tracks.rs`.
   - The sample picker (`sdk/src/sample_picker.c`, linked into each sampler module) keeps the module's pool list (entry index to path) in the module's project scope. On a project change and on pool lost, it first calls `sample_pool_set` for each entry of the new list, then `sample_pool_clear` for entries the list does not use, so files shared by both projects keep their entry (the order in sample-api-1.5 section 3). The re-drive must be idempotent.
   - Contract note: `sdk/include/dk/sample_track.h:74` ("the pool is kept per project") changes meaning. The SDK is 0.x, so a minor version may break; SHOT is the only user.
   - Result: new modules that need saved state use the general API and need no runtime change.
4. **Write on Elektron save.** Project-scope data is held in RAM and written to the `.dkp` when the stock project save succeeds. Changes the user discards (load without save) are discarded for digikit too. Fallback if the save hook fails the byte check: write 2 s after the last change, like the settings record.
5. **Identity.**
   - Key: (device, slot). The `.dkp` path is slot-based, so it survives renames: `digikit/project/<device>-<slot>.dkp` (draft; step 2 fixes the path within the store path rules: at most 159 bytes, printable ASCII).
   - The `.dkp` header stores the project name and the save stamp: the 32-bit word at card slot header +0x18, which stock writes on each save. It is a save stamp, not an identity (finding 110 and finding 39 say the word is not an identity; both are true).
   - On load, compare the `.dkp` stamp with the listing record's stamp for the slot, before the load regenerates the live stamp. Match: the `.dkp` belongs to the saved Elektron state; if the name differs (a stock rename keeps the stamp), update the name in the `.dkp`. Mismatch (stock save-as or overwrite without digikit, an older card): warn and ignore the `.dkp`.
6. **Validation on load.**
   - Modules missing from the card: one short warning in the firmware font. The plain Elektron project loads. No error screens.
   - Modules on the card but not selected: offer "load + restart" as one button ([one-button flows]).
   - Hot load of modules is a later item.
7. **Layers. No new user-facing module.**
   - `modules/core/adapters/project` (new): hooks on the slot setter and on save, both devices. Anchors in `devices/fw/project.toml`.
   - Runtime `project` capability: reads and writes the `.dkp`, validates, sends events to modules.
   - `modules/core/apps/digikit-menu`: the warning and the "load + restart" offer.
   - The store writer belongs to the `store_upload` capability today; a capability may use another only through its declared API (module-architecture rule 3). Step 2 decides the declared store-write API that `project` uses, or moves a store-write API into the core next to the reader.
   - The `.dkp` writer reuses the card checks of settings writes (`block_write`, finding 46: a foreign card refuses, a blank card is claimed first).
   - A save during an upload: the upload uses shared RAM buffers (`IN_AT`, `OUT_AT`, `SCRATCH_AT`). Step 2 decides queue or retry, and the RAM size of the in-memory `.dkp`.
   - New adapter callbacks (`dkrt_project_changed` moves from `at = "tracks"`, plus a new `dkrt_project_saved`) get `abi.toml` entries with an `at` target for the `project` capability, and the generated `gen_abi*.rs` files are regenerated. Update module-architecture rule 4, which lists the adapter callbacks.
   - The hooks only read stock state. Do not change stock project storage or the `Value<T>` project-data classes (ask Em before touching storage).
8. **Later, not in this plan.**
   - Elektron project data inside the `.dkp` (maybe, after this lands).
   - A workbench bundle export: Elektron project plus `.dkp` as one backup file (host side).
   - Hot load of modules.

## Firmware facts

Static reads on DT2 OS 1.17 and DN2 OS 1.12, 2026-11-02. [D] from one read on 2026-11-02, then checked by a second agent the same day against disassembly: all rows confirmed, with the corrections included here. Step 1 still checks the rows marked unchecked below and updates finding 110. Commands: `uv run python -m tools.coldfire.fw {decomp|disasm|callers|xref} ADDR -i dt2-1.17|dn2-1.12`.

| Item | DT2 1.17 | DN2 1.12 |
| --- | --- | --- |
| Current slot byte | `0x4099d5c5` | `0x405ce899` |
| Slot setter (writes only on change; DT2 notifies via `0x400c102c`) | `0x400c11ca` | `0x400bb7fa` |
| Setter wrapper `(Project*, slot)`: `if Project::vfunc(0x28) != 0 then setter(slot)` | `0x400424a2` | `0x4003eb62` |
| Slot getter (`0xffffffff` when no project) | `0x400424ca` | `0x4003eb8a` (reads the slot byte through `0x400bb868`) |
| `Project*` global | `0x44f370a0` (set by `0x40198b7e`) | `0x446488d0` (set by `0x4018ac5a`) |
| Live `project_t*` | `*(Project+0x10)` (`Value<project_t>::vfunc_10` at `0x401a61ea`) | same (vfunc_10 at `0x40195b5a`) |
| Live name: 16 bytes ASCII, zero padded, bytes >= 0x80 stored as 0, no guaranteed NUL | `*(Project+0x10) + 0xc` | same |
| Name writer `ProjectDataChangeInfo(Project*, src)`, flag 0x20 | `0x40042140` | `0x4003e800` |
| Live stamp (built by `0x401524ce` / `0x4015097e` from two 15-bit LCG steps `0x401524a0` / `0x40150950`; bits 31 and 15 are always 0) | `*(Project+0x10) + 8` | same |
| Stamp regenerated by 1-arg `ProjectDataChangeInfo`, flag 0x80, on each load, new and load from memory, and at boot when 0 | `0x40041c96` | `0x4003e38c` |
| Save `(Project*, slot, progress)`, nonzero on success. Slot 0x80 is the live mirror: skip it | `0x40043a30` | `0x40040190` |
| Save / save-as wrapper `(Project*, slot or -1, progress)`: setter(slot) (or the getter for -1), then `FUN_40043b28(slot)` (rejects slot > 0x80, calls the save), then save slot 0x80, then setter(slot) again, then `FUN_40042522(Project*, 0)` | `0x40043b78` | `0x400402d8` |
| `loadProjectFromMmc` (does not call the setter itself) | `0x40046906` | `0x40042b92` |
| Load wrapper (calls the setter). Order: load, save slot 0x80, setter | `0x40046c38` | `0x40042e6e` |
| `initializeNewProject` | `0x400465e2` | `0x400428c0` |
| `loadProjectFromMemory` | `0x40043f74` | `0x400403d6` |
| Per-slot listing record at `MmcFs + 0x10 + slot*0x1c` (slots 0..0x80): valid byte +0, name +8..+0x17, save stamp +0x18 (copied from card header +0x18 by the listing refresh DT2 `0x4012f2fa` / DN2 `0x4012dbce`, which also runs after each save). `FUN_4012f048(MmcFs, slot)` returns record+8 (or the static `0x44e406d4` when invalid) | `0x4012f048` | `0x4012d91c` |
| Live to card header copy (flag 0x80 writes header +0x18 = live stamp) | `0x400df66c` | `0x400e15f8` |

When the setter runs:

| Action | Setter runs |
| --- | --- |
| Load | Yes, in the load wrapper. Name and stamp are in place before it. |
| New | Yes (unchecked). |
| Load from memory | Yes (unchecked). |
| Save-as to another slot | Yes, before and after the write. Name unchanged. |
| Save to the current slot | No change (the setter is a no-op). |
| Delete | Only if the deleted slot is current: setter(-1) (unchecked). |
| Format, reset, `project_container_decode` | setter(-1) (unchecked). |
| Rename (DT2 `0x400a1246`, DN2 `0x4009b3bc`) | No. Updates the live name when the slot is current. |
| Rename in the +Drive browser (DT2 `0x400e839a`, DN2 `0x400e9d6a`) | No. Patches the card name and the listing only, not the live name (unchecked). |
| Copy or move in the +Drive browser | [O]. Leads: `0x400a131e` and `0x400a1376` call the card write `FUN_400f5150` without the setter. |
| Power-down autosave | [O], none found. |

Correction to [finding 110](../findings/110-project-identity.md): `loadProjectFromMmc` does not call the setter; its wrapper does. Step 1 updates the finding.

## Storage

- The settings record (8 KiB per copy, value at most 1024 bytes, shared with `fx/route/<id>` and `fx/send/<id>`) is enough for module scope. It is too small for project scope: one pool list is 16 paths of up to 159 bytes, about 2.6 KiB.
- The `.dkp` is a store file. Store format and table: [digikit-store](../design/digikit-store.md), code in `modules/core/runtime/sdk/src/store.rs`. Today only the upload path (`modules/core/runtime/src/rs/src/upload.rs`) writes store files. A runtime-side writer for `.dkp` files is the main open risk of this plan.
- Draft `.dkp` layout (step 2 fixes it):
  - Header: magic, version, device, slot, name[16], save stamp, seq.
  - Module list: module id and hash for each needed module.
  - One section per module id: key/value entries in the same entry format as the settings record.
  - Header also stores the device id, so a card moved between devices is detected.
- Write only on save, so card writes stay rare.

## ABI and versions

- New `dk_data_*` entries are appended to the `Api` table: `[contract] api` goes from [1,6] to [1,7] in `sdk/sdk.toml`, each with `since = 7`.
- The changed meaning of `sample_track.h:74` plus new entries is a runtime 0.x MINOR (now 0.5.4; module-architecture section 12).
- A store format change (the `.dkp` file) is also a MINOR.

## Open questions

1. What counts as a needed module. Proposal: a machine module used by any track, or an FX module with an active route. Fallback: the modules selected while the project was active.
2. Rename: handled by the stamp match (decision 5). Check that a +Drive rename of the loaded project behaves the same as a menu rename.
3. Copy and move of projects in the +Drive browser.
4. FX routes stay global in the settings record unless Em wants them per project.
5. DT2 stock project load also resets the borrowed pool slots ([finding 109](../findings/109-dt2-stock-pool-borrowing.md)). The re-drive comes from the module (decision 3), on pool lost and on project changed. Check the order of the two events.
6. Power-down autosave: none found; confirm.
7. A failed stock save: the save returns a nonzero byte on success; write the `.dkp` only on success.

## Steps

Estimate: about 1.5 days of agent time. Main risk: the store file writer.

1. [ ] Byte check of every firmware fact above by a second agent, both devices. Resolve the [O] cells needed by step 3. Update finding 110 and mark checked facts [V]. Unchecked: setter calls in `initializeNewProject` and `loadProjectFromMemory`, delete/format/reset rows, +Drive rename addresses, copy/move. Update finding 110: the name lookup is resolved (live+0xc, 16 bytes); loadProjectFromMmc does not call the setter; the +0x18 word is a save stamp. (2 h)
2. [ ] Design doc `docs/design/digikit-project.md`: `.dkp` format, C signatures of the module API, events, error codes, size limits. Em's OK before code. (1 h)
3. [ ] `core/adapters/project`: hooks on the setter (DT2 `0x400c11ca`, DN2 `0x400bb7fa`, on change; stack argument, -1 = no project) and on the save (DT2 `0x40043a30`, DN2 `0x40040190`; skip slot 0x80; act only on a nonzero result), both devices; anchors in `devices/fw/project.toml`. Calls the existing `dkrt_project_changed` and a new `dkrt_project_saved`. (0.5 day)
4. [ ] Runtime: store file writer; read the `.dkp` on project change, write it on save; `dk_data_*` API and project event in `sdk/sdk.toml` (API minor). Module scope uses the settings record. (0.5 day)
5. [ ] Pool: remove the per-project lists from `sample_tracks.rs`; the sample picker keeps its list in project scope and re-drives on change and on lost. (3 h) Docs to update: sample-api-1.5 (lines ~45, 65, 116-118, 137-138, 168 and its TODO on store persistence), `sdk/include/dk/sample_track.h:74`, the `sample_tracks.rs` header comment and the tests calling `project_changed` (`sample_tracks/tests.rs`), module-architecture 3.10 and section 12, digikit-store "Keys by owner" and the line that only upload writes the store.
6. [ ] Validation: missing-module warning and "load + restart" in `digikit-menu`. (2 h)
7. [ ] Tests: host unit tests for the `.dkp` format and the API; one emulator scenario per device: load samples, save, power cycle, reload, pool restored; a project whose module is missing shows the warning and loads. (3 h)
8. [ ] Later: move FX send amounts and F4 effect values out of kit spare halfwords into the `.dkp` (per kit).

## References

- [digikit-store](../design/digikit-store.md): store layout and the settings record.
- [module-architecture](module-architecture.md): layers, D12, D13, D19, section 3.6.
- [sample-api-1.5](../design/sample-api-1.5.md): pool API and project handling today.
- [finding 110](../findings/110-project-identity.md): project identity.
- [finding 109](../findings/109-dt2-stock-pool-borrowing.md): DT2 pool borrowing and stock resets.
- [finding 63](../findings/63-settings-store.md): settings store.
- `~/src/digi/WORKBENCH-AND-MODULES-PLAN-2026-10-19.md` (outside the repo): the earlier `.dkp` item.

[one-button flows]: ../design/ui-rules.md
