# What fits: tell the user before a load whether a module will load (plan, 2026-10-23)

Status: all five steps built on the lane branch (merged), then reworked to infer the needs instead of declaring them (results: docs/findings/60-what-fits.md). Follow-up on the lane branch memory-panel (below): storage-only Transfer page, a Memory panel, a load line on the device, new labels.
Lane `tasks-images`, base main 38dfbe4a.

Today a selection that is too big is refused at boot and shown afterwards (a RESULT row, the log error `E_DSP_NOSPACE`). The user should see it before
selecting or installing. Em's brief for the runtime UI: load and unload, health, storage, what can and cannot load.

## Design (approved by Em)

1. **Inferred needs.** (First built as declared needs in a format 2 header; changed on Em's decision, 2026-10-23: a module must not self-report its needs. A contributed or
   hand-built `.dkm` has no field to lie in, and the runtime enforces the limits at load anyway.) One function, `needs_of(source)` of the crate `packages/dkneeds`, works out from
   the file's own contents what a load takes: arena bytes, the relocated DSP image's arena bytes, DSP area bytes and alignment, machine, voice, parameter and page rows, FX slots
   (docs/design/digikit-dkm.md, "Needs are inferred from the contents"). It reads through a byte source: the device feeds it from store sector reads (header, `register` records, DSP header
   and voice table; nothing is staged), a host from a slice. Malformed input is an error, never a panic. The `.dkm` format stays at 1. Callers: the DIGIKIT menu, patchkit
   (`patchkit needs`), patchkit.wasm (`pk_needs`, for the Transfer page), the Python tools through the command line. No other copy of the arithmetic.
   Test: the `inferred needs` tests of `modules/core/runtime/tests/capacity/capacity.device.test.ts` load a set of run-time modules and fixtures alone (coupled) and compare CAPACITY and MODULES
   with `needs_of` of the file; Rust tests of `dkneeds` cover malformed files.
2. **One rule.** `fits_now(needs, capacity)`: each need is at most the free amount of its resource (the arena and the DSP area against the largest free
   block, rows and slots against limit - used, a module record and a selection path free, the file within the staging buffer). The compare is written in Rust (`dkrt_fit`, the menu),
   TypeScript (host) and Python (tools); shared test vectors keep the three equal (the needs are computed once, in Rust). A sum rule
   for several modules takes each module's needs out of the capacity in turn (conservative: fragmentation is not modelled).
3. **Device MODULES list.** Each row shows the memory the module takes (`259K`) and `x` as the fourth character when it will not fit next to the ticked ones (the check starts from the limits, since the next boot
   is empty); selecting such a row ticks nothing and says which resource is short (popup `NO FIT: DSP 164K > 155K FREE`, short form in the message row). The rows are mirrored in `DK_LOAD`; test `modules/core/apps/digikit-menu/tests/fits.device.test.ts`.
4. **Host.** Transfer page (needs from `pk_needs` of patchkit.wasm): tag `fits` or `won't fit` per library module (live CAPACITY; the tooltip says whether it fits alone) and a line for the ticked set (the sum rule against the limits); `dk run` refuses a module that does not fit
   unless `--force`; `module:check` prints the needs and whether the module fits an empty device.
5. Docs (`digikit-dkm.md`, `module-sdk.md` 6.5, finding 60) and tests.

## Facts used

- The loader reserves, in order: the DSP range (`dsp.rs` `extent`, granule 64, first fit by alignment), the arena block of the relocated DSP image
  and the voice rows, the arena block of the ColdFire image and bss (`E_NOMEM`), then the registry rows (`E_PARAM_FULL`, `E_PAGE_FULL`).
- The module set is fixed per session (hot reload is archived): a ticked set is loaded by the next boot into an empty runtime. A fit check for a
  selection therefore starts from the device's limits and takes the needs of the other ticked modules, not from what is loaded now.

## Open

- Fragmentation is not modelled (conservative). The DSP area's first range starts at an offset that is not 8-aligned on the Digitakt II (finding 57): the empty DSP area is taken as 8 bytes short, which covers it.
  Machine type numbers (the sticky map) and a module's own `init` allocations are not modelled.
- The device list has no room for more than about 18 characters a row (finding 60): names are cut to 8 characters.
- The menu reads the needs of every file each time MODULES opens (about one sector read per file; finding 60). A cache across opens is not needed at this cost.

## Follow-up: the Memory panel, the load line, the labels (approved by Em, 2026-10-23)

1. **The Transfer page is storage only.** Modules mode keeps each row's file size and adds one storage line to the On device column, `Modules: <n> files, <size> · <free> free` (the store's free space,
   GiB from 1 GiB). No memory wording is left there except the fit tag, whose tooltip uses the new names: `CPU memory 3 KiB short`, `DSP memory 40 KiB short`.
2. **The Memory panel** (`MemoryPanel.tsx`, `memory-core.ts`, `memory-backend.ts` in `apps/desktop/ui/src`): a collapsible panel in the Card panel's look, under the Card panel in one right-hand column of the Emulator page (`SidePanels.tsx`; both collapsible, a closed one keeps its title bar,
   the column shrinks to a rail when both are closed) and in the same column of the Remote page (the target there is the emulator or a MIDI port). One bar per resource the device limits (DSP memory, CPU memory, Machines, Parameters, Pages where the device has page rows, Modules), split into one
   segment per loaded module, coloured by the module's type (machine, effect, tool; the site's `--amber`, `--teal`, `--ok`), then free space. What `used` holds beyond the known modules is one `Other`
   segment; hover a segment for the module and its amount; `used / limit` at the end of the bar (KiB for bytes). The data is CAPACITY and MODULES of the device and `needs_of` (patchkit.wasm) of the matching
   library file; a module takes its own arena block only (the block of its DSP image is freed after the transfer: finding 60). The panel reads when it is shown, when a Transfer job ends, after a restart, on focus
   and with its Refresh button, one read at a time over the target's one session (`read-gate-core.ts`, `session-core.ts`); a hidden or collapsed panel reads nothing. Nothing pending or ticked is drawn.
3. **The load line on the device** (`dkrt_fit::load_line`, above APPLY & RESTART, mirrored in `DK_LOAD`): the DSP and the CPU memory of the ticked set, always these two, `DSP 85% CPU 1%`. Test vectors in `sdk/vectors/fit-vectors.json`
   (`load_lines`), the Rust rule, and the `fits.device.test.ts` test.
4. **Labels.** Desktop: `CPU memory` (the arena) and `DSP memory` (the DSP area); device: `CPU` and `DSP` (the MEMORY page's `ARENA` row is `CPU`, the fit texts say `CPU` for `RAM`). The protocol and code names stay (CAPACITY kind
   `ARENA`, `arena.rs`, `DSP_AREA`).
