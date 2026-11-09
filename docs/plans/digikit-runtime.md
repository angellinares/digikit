# digikit runtime: load modules from the +Drive at run time

Status: design agreed 2026-10-16; **changed 2026-10-17 to boot-time loading** (decision 9). Phases 0 to 5 built for hot loading (tag `digikit-hot-reload-v1`, archived in docs/plans/digikit-hot-reload.md); Phase 6 is the current work. Branch `runtime/digikit-runtime`; commits after a green verify (Em, 2026-10-16).

## Goal

A resident runtime ("digikit runtime", dkrt) inside the Elektron MAIN OS. The user opens SETTINGS > DIGIKIT >
LOAD MODULES, ticks modules found on the +Drive and presses LOAD. The runtime loads them into memory that the
firmware never uses, without re-flashing. Later items in the same menu: health and diagnostics (CPU and RAM
headroom on the real device), and faster transfer of modules and samples.

The first target is the emulator only: copy modules into a `plus_drive/` folder, boot `mise run emu`, load them
from the menu. No hardware flashing until everything works in the emulator.

## Decisions (2026-10-16)

1. The runtime is installed once with patchkit, as a module on top of `core/expansion`. Every patch to firmware
   code is static and made at patch time. Loading a module never rewrites firmware code: it only fills slots and
   registry rows that the runtime owns.
2. On-device language: Rust through `rustc_codegen_gcc` (GCC's back end with `-mcpu=54418`; S1b), behind a C
   ABI. C (m68k-elf gcc, tools/patchkit/pk_toolchains.py) is the fallback. Module interfaces avoid small-struct
   results. Hook shims stay in assembly; DSP code stays SHARC assembly (no Rust back end for SHARC).
3. v1 scope: ColdFire-only modules load and unload at run time. Machine modules load at run time on both
   devices. DSP code goes in the unused tail of the per-block frame (S4b): DT2 698 bytes and DN2 60 bytes per
   block. Restart-only is the fallback.
4. The `plus_drive/` folder is the source of truth for the emulator's +Drive: the image is built from it at boot,
   and the files the firmware writes are copied back to it on exit.
5. Name: "digikit runtime" (dkrt), not "OS". It runs as a guest inside Elektron's OS.
6. `plus_drive/` layout: `plus_drive/dt2/` and `plus_drive/dn2/`, one per device, ignored by git. Files and
   folders map to the path filesystem as they are: a `.wav` is converted to a native sample, any other file is
   copied byte for byte. `.mmcfs/projects/`, `.mmcfs/sounds/` and `.mmcfs/kits/` hold raw MmcFs slots, one file
   per slot (`NNNN.bin`). The exporter writes changed files back byte for byte and keeps each replaced file under
   `out/plusdrive/backup/`.
7. Module store: card sectors 0x38000-0x3E000 on both devices (S6; **superseded 2026-10-22**: a per-device area above the +Drive on the DT2 and above 0x800000 on the DN2, `docs/design/digikit-store.md` version 3, finding 59), read with the firmware's own block read
   function. `plus_drive/<device>/digikit/` maps into it. The DT2 path filesystem stays for samples.
8. Device facts are data: what differs between devices (card layout, store range, panel, DSP link, frame
   channel sizes) lives in `devices/<device>.toml`, and firmware-version addresses in `devices/ports/*.toml`.
   Code takes a profile, never a device enum, so another device on the same platform (for example Syntakt) is a
   new TOML file. The card layout moved there first (`[card]`, packages/emulator/device-profile).

9. **(2026-10-17, replaces the run-time load/unload of decision 3.)** The module set is fixed for a session. The user
   ticks modules in DIGIKIT > MODULES and chooses APPLY & RESTART; the runtime saves the selection and restarts, and
   at boot it loads the selected modules before the firmware builds its machine tables (finding 36). No unload, no
   load while a project is open or playing. Reason: a load/unload under an open, playing project needs a transport
   guard, an in-use scan of the whole project, ordering under concurrent readers and a soak to be trusted (the risk
   review of 2026-10-17); a restart removes that class. Hot loading is archived (tag `digikit-hot-reload-v1`,
   docs/plans/digikit-hot-reload.md) and may return later, first for code-only modules.
10. The permanent identity of a run-time machine is its type number (16 to 23, `modules/core/machines/types.toml`):
    kit tracks and pool sounds store it. Parameter codes may be allocated first-free each boot: sounds and p-locks
    are keyed by sound-array slot (finding 36, question 4).
11. Getting it onto a device, in order: flash the patched OS once through Elektron's own OS update (SysEx; patchkit
    writes only sections 3 and 7, so the stock OS can always be re-flashed: finding 36); upload modules over USB
    SysEx into the store (the runtime's only card write besides the selection record); USB mass storage ("disk
    mode") is assessed later.

## What is known (sources)

| Fact | Mark | Source |
| --- | --- | --- |
| patchkit (packages/patchkit) is Rust; it is built to `apps/web/public/patchkit.wasm` for the browser; the CLI runs it through cargo; the desktop app only reads the patch record | [D] | tools/emulator/native_wasm.py, tools/patchkit/patchkit.py, packages/emulator/boot/src/runtime.rs |
| Module objects are relocatable; patchkit links them to absolute addresses per image at patch time; no run-time relocation exists | [D] | modules/README.md, docs/findings/20-patch-toolchains.md |
| DDR is 128 MiB at 0x40000000, cacheable copyback through ACR0; 0x48000000 is the uncached alias | [V] | docs/findings/01-container-and-patching.md, docs/findings/08-hardware-and-ghidra.md |
| Free RAM `cf.expansion`: DT2 1.16/1.17 0x47e8b000-0x47fe0000 (1.33 MiB), DN2 1.11 0x46712000-0x47fe0000, DN2 1.12 0x4671a000-0x47fe0000 (24.8 MiB) | [D] | docs/findings/01-container-and-patching.md (RAM map) |
| That region was checked for boot, navigation, play and an idle soak only; save/load, sample record and delete, SysEx, USB and OS update are not covered | [O] | docs/findings/01-container-and-patching.md |
| Firmware heap: one buddy allocator, 32 MiB in bss (`FUN_4011e674` alloc, `FUN_4011e87a` free) | [D] | docs/findings/01-container-and-patching.md |
| Code runs from DDR; the I/D cache is not coherent on hardware; the emulator is coherent and hides a missing flush | [D] | docs/findings/07-emulator.md |
| The DSP image goes to the SHARC once at boot (`FUN_400ccf74` on DT2 1.16, over DSPI0); no later code path to the DSP is known | [O] | docs/findings/06-sharc-engine-and-startup.md |
| Free SHARC L2: DT2 from 0x2001ab7c, DN2 from 0x2008823c, to 0x200fe000 | [D] | docs/findings/06-sharc-engine-and-startup.md |
| Machine tables: `core/machines` copies the stock tables with added rows into `cf.expansion` (RAM); type numbers are permanent; unselected numbers are gap rows | [D] | modules/core/machines/types.toml, docs/findings/02-machines-and-parameters.md |
| +Drive format: Elektron's own filesystem (not FAT): MmcFs pools for projects, sounds and kits, plus a path filesystem for samples | [D] | docs/findings/14-plus-drive-format.md |
| Firmware file functions: path walker `FUN_40159f50`, readdir `FUN_40157072` (`FileSystemDirectory`), inode fetch `FUN_4015b64e`, `File::write` `FUN_4015838e` | [D] | docs/findings/14-plus-drive-format.md |
| The emulator card is copy-on-write: writes go to an in-memory sector overlay and are lost on exit | [D] | packages/emulator/card/src/lib.rs |
| UI: views are C++ classes; `VerticalMenuView` exists; no SETTINGS menu table and no text renderer are documented | [O] | docs/findings/03-ui-and-panel.md, docs/findings/07-emulator.md |
| Elektron Transfer uses SysEx over USB MIDI; no USB mass storage is known | [D] | docs/MIDI-SYSEX-RPC.md |

## Design

### Runtime (installed by patchkit)

- A new patchkit module `core/runtime` that requires `core/expansion`. Its code and data live in `cf.expansion`.
- Static parts, placed at patch time:
  - Hook trampolines at the firmware sites the runtime needs. Each one jumps through a function-pointer table in
    RAM. An empty slot runs the stock code.
  - The DIGIKIT entry in SETTINGS and its views.
  - An import table for this image: firmware name to address (file functions, allocator, UI classes, cache
    flush), resolved from the port lockfile like any anchor.
  - The module arena: a sub-region of `cf.expansion` with its own allocator, outside every region patchkit
    places static objects in.
- Dynamic parts, filled at run time: the slot table, registry rows (machines, params, pages) and the loaded
  module list.

### Module file (`.dkm`)

- Header: magic, format version, size, sha256, module name and version, required runtime version.
- Sections: code, data, bss size.
- Relocations (cf.32 and the other `.dko` types) against the module base and against imported firmware names.
- Exports: `init`, `fini`, slot bindings and registry rows.
- No firmware addresses inside, so one file works on every image whose runtime provides the imports it names.
- Built by the Rust host tooling from the existing `.dko` objects (a `pack` step next to patchkit's linker).

### Load sequence

1. Read the module from the store (the device's `[card.store]` area) with the firmware's block read function (DT2
   `FUN_4012deda`, DN2 `FUN_4012c59a`).
2. Check the header, sha256, runtime version and that every import resolves. Nothing is written before this
   passes.
3. Allocate in the arena, copy, apply relocations, zero bss.
4. Flush the ColdFire caches for the range (CPUSHL).
5. Call `init`. Then publish each slot binding with one aligned 32-bit store.

Unload (ColdFire-only modules): clear the slots, wait until no task is inside the module (an entry counter the
trampolines maintain), call `fini`, free the arena block. A module that a project references (a machine) is not
unloaded while running.

### Safety rules

- On hardware, nothing loads at boot by default; a key held at boot skips any autoload (safe mode).
- The arena is used only after phase 2 shows that no firmware path writes it.
- A module that fails a check is refused with a message; the runtime never runs a partly loaded module.

### +Drive dev loop

- `plus_drive/` replaces `samples/` as the input to `mise run emu` (samples, modules and any other files).
- At boot: build the image from the folder (packages/emulator/plusdrive). Cached by content, as today.
- On exit: read the image plus the card overlay and copy back what the firmware changed, byte for byte:
  path-filesystem files in their native format, and MmcFs project, sound and kit slots as raw blobs. The builder
  accepts these blobs again unchanged, so the round trip is lossless without decoding the sample trailer or the
  project format.
- Builder extensions needed: directories larger than one content page, files with more than one extent.

## Phases

### Phase 0: spikes (static where possible; run in parallel)

- [x] S1 Rust on ColdFire. Result: stock rustc/LLVM emits 68k-only forms (11 to 27 rejected lines per build).
- [x] S2 SETTINGS menu. Result: the item list is rebuilt at run time by `FUN_400a59a0`; one hook at its tail adds
  a DIGIKIT item, and our view is a stock `VerticalMenuView` (about 70 % confidence for the first run). Same
  structures on DN2.
- [x] S3 File reads from our code. Result: a locked C-level API (open, read, size, close, directory iteration)
  callable from the UI task on DT2. No path filesystem found on DN2.
- [x] S4 ColdFire-to-SHARC path after boot. Result: DT2 can upload bytes to DSP DDR through the sample link-port
  path. DN2: no existing channel (S4b found one of our own).
- [x] S1b `rustc_codegen_gcc` for ColdFire. Result: zero rejected lines, ColdFire ELF flags, only `R_68K_32`,
  the same calling convention as gcc except 8-byte struct results; the repo's libgcc helpers are 68020 code.
- [x] S4b A DSP channel of our own. Result: the unused tail of the per-block frame on both devices (DT2 698 bytes,
  DN2 60 bytes per block), with a full-frame borrow as a faster, muted option.
- [x] S6 Card use. Result: a module store at sectors 0x38000-0x3E000 on both devices (superseded 2026-10-22 by the per-device areas of store version 3, finding 59); FsRaw is not dispatched
  on either device.
- [x] S5 Instrumentation. Result: write-watch and flush check in the emulator; a DT2 boot shows 0 writes into
  `cf.expansion` and 0 flush hits.

Gate: done (docs/findings/23-digikit-runtime-spikes.md).

### Phase 1: `plus_drive/` dev loop (host only)

- [x] 1a: `plus_drive/<device>/` folder and `mise run emu` pointing at it (ignored by git). Builder: nested
  directories, plain files, multi-page directories, inline and indirect extents, raw MmcFs slot import
  (docs/SAMPLES.md, finding 14). Checked through the firmware's own mount, readdir and read
  (`mise run plusdrive:check`).
- [x] 1b: image reader (path filesystem and MmcFs slots) and exporter (`plusdrive export`, console `card.export`),
  with backups (docs/findings/24-plusdrive-export.md). A panel project save arrives as
  `.mmcfs/projects/0000.bin`.
- [x] The builder takes the exporter's `.mmcfs/boot/SSSSSS.bin` (the copies at 0x40000 and 0x48000 and the
  working project at 0x50000); a boot copy in the folder replaces the generated boot project (docs/SAMPLES.md).
- [x] `mise run emu` exports on exit (`--card-folder`, log line `card: exported N changed files to ...`; `-- --no-sync` turns it off).
- [x] The desktop "Choose samples" path builds from a `plus_drive/` folder (nested folders, `.mmcfs/`, `digikit/`).
- [x] The builder maps `plus_drive/<device>/digikit/` into the module store (docs/design/digikit-store.md); read back through the firmware's block read on DT2 and DN2 (`mise run plusdrive:check`).
- [x] The export passes the running device; the card layout is device data (`[card]` in `devices/*.toml`).
- [x] MmcFs bitmaps: they are write-protect bits (set = padlock; the save path reads them and answers `N/A: Write-protected`), not occupancy. The builder set them for imported slots and so locked them; fixed, with a test (finding 35). Open: export protection a player sets in the emulator (sidecar file).

Gate: a project saved and a sample recorded in the emulator are in `plus_drive/` after exit and are there again
after the next boot.

### Phase 2: arena safety

- [x] Scenarios for the paths not yet covered: project save, load, new, delete, rename, format, OS update (enter and
  cancel), the filesystem check, MIDI DIN input, MidiRpc over the USB callback (docs/findings/25-digikit-safety.md).
  Not covered: sample record and delete (no audio input in the emulator), the startup menu (FUNC held at power-up), real USB.
- [x] Run them with the S5 write-watch on `cf.expansion` and with the canary (tools/coldfire/arena_safety.py): 0 write
  events, 0 flush hits, canary intact, on DT2 1.16 (16 runs) and DN2 1.11 (15 runs).
- [x] Static check of the arena with tools/coldfire/refscan.py and tools/coldfire/cf.py (`arena_safety static IMAGE`): no absolute
  reference into `cf.expansion` (either view) or to the module store sectors on dt2-1.16/1.17 and dn2-1.11/1.12; coverage
  96.6 to 96.8 %; every hit read as a constant or data (finding 25). Computed addresses are covered by the dynamic runs only.
- [x] The card overlay after each scenario has no sector in the module store (then 0x38000-0x3E000; today the device's `[card.store]` area, same check).

- [x] DT2 1.17 and DN2 1.12 suites, the 1.16/1.11 re-run, and one suite per image with `core/runtime` patched in (ALLOW_PC = its
  range): all PASS, 0 write events (finding 25, second pass).
- [x] MidiRpc write calls: the FsSample file write runs and writes nothing in the store; the Data writes are refused without an
  active transfer (open: that transfer state).

Gate: no write into the arena by firmware on any scenario; result recorded in docs/findings/25-digikit-safety.md. Open
before the arena is used: sample recording and delete (no audio input in the emulator), the Data write RPCs.

### Phase 3: runtime v0 (no UI)

- [x] Toolchain: `pk_toolchains codegen-gcc` (pins, patches and the target JSON under tools/patchkit/toolchain/);
  Rust modules take ColdFire-safe helpers from `compiler_builtins` (docs/findings/26-rust-on-coldfire.md). Open:
  an install from scratch, timed.
- [x] C modules: a libgcc built for `-mcpu=54418` (`pk_toolchains libgcc-cf`, under a minute), linked into a C module as local symbols by `pk_build`; the ISA checker `tools/coldfire/isacheck.py` (no committed object has a 68020-only form); `test/c-demo` 64-bit divide, remainder, multiply and shifts correct through the runtime on dt2-1.17 and dn2-1.12 (finding 35).
- [~] Module interface, schema and Rust SDK, designed together. Schema part done (finding 27): typed `[[machine]]`,
  `[[param]]`, `[[page]]`, `[[voice]]` sections with per-device knob models, generated Rust constants, SHARC include and
  gallery JSON; FX and MenuItem sections and the run-time registry interface are open. One schema (Machine, Param, Page, FX, MenuItem)
  generates the runtime's registry rows, the SDK types, the constants that SHARC assembly includes and the
  gallery metadata. A versioned runtime interface through which a loaded module registers what it adds.
- [x] One Rust module linked by patchkit and run in the emulator (`test/rust-demo`, all four ports).
- [x] `core/runtime` module: UI-loop hook trampoline, import table, arena allocator, store reader, loader (docs/design/digikit-runtime.md, finding 29). Slot 0 only; no registry yet.
- [x] `.dkm` packer in Rust (`patchkit pack`, docs/design/digikit-dkm.md).
- [x] A ColdFire-only test module: `test/dyn-demo`, a counter in slot 0.
- [x] Loading triggered from the emulator by a mailbox poke (`mem.cf.poke`; `tools/patchkit/pk_dkrt.py proof`). No autoload folder.

Gate: load, run and unload in the emulator on DT2 1.16 and DN2 1.11, write-watch clean, no flush warnings. Done on DT2 1.16/1.17 and DN2 1.11/1.12 (finding 29); open: the interface/schema items above (the C-module libgcc is done, finding 35).

### Phase 4: DIGIKIT menu

- [x] SETTINGS > DIGIKIT entry (`modules/core/digikit-menu`, Rust): a hook before the selection restore at the end of the SETTINGS
  rebuild (DT2 `0x400a5d48`, DN2 `0x4009fe24`) adds a `MenuItem`; its action pushes a stock `VerticalMenuView` (LOAD MODULES, ABOUT).
  LOAD MODULES lists the `.dkm` files of the card store as `[ ]`/`[X]` rows (a label rebuild, not a draw callback), YES ticks, the
  last row LOAD calls `dkrt_load` through the table `DKRT_API` that the runtime fills (no runtime: NO RUNTIME). Proof in the emulator on
  all four images (docs/findings/28-digikit-menu.md, `tools/coldfire/digikit_menu_proof.py`).
- [x] Joined to the runtime (finding 28, "Joined to the runtime"): the menu `requires` `core/runtime` and calls `dkrt_load`, `dkrt_unload`,
  `dkrt_status` and `dkrt_module` by symbol (no `DKRT_API`, no stand-in module); `DkrtStatus`, `DkrtModule`, result codes and the card
  store reader are one definition in `dkrt-sdk`. Rows show `[X]` and a loaded marker; LOAD loads ticked and unloads unticked loaded
  rows; errors show the runtime's result text; ABOUT shows ABI, loaded count and free KB.
- [x] Rust builds do not depend on the checkout path (`pk_rust.stage_crate`; finding 29 update).
- [x] A popup for errors (the firmware's own PopupWindow, reached through six call-operand anchors), an UNLOAD item separate from the ticks,
  the file name in the failure text, a text for each result code (unknown: `ERROR <n>`), a loading state for asynchronous loads (reads lane A's
  `state` and `LOADING`; not yet run against an asynchronous runtime), greyed modules for another device or a newer runtime. Four images, 4 scripts
  each, 0 stray writes, 0 flush hits (finding 34, `digikit_menu_proof run --quick`).
- [x] UI-task stack: 160 KiB on all four images (task-create calls, `tools.coldfire.task_stacks`), 3256 bytes used in a load, popup and unload
  (`DIGI_STACK_WATCH`, `docs/INSTRUMENTATION.md`; finding 34): a load may run on the UI task; no dedicated stack needed.

Gate: done. The dyn-demo module loads from SETTINGS > DIGIKIT > LOAD MODULES on DT2 1.16, DT2 1.17, DN2 1.11 and DN2 1.12; its counter
rises; ABOUT shows loaded 1; the row is marked; unticking and LOAD unloads it, the counter stops and `fini` ran; ABOUT shows loaded 0;
write-watch 0 events, flush check 0 hits (`tools/coldfire/digikit_menu_proof.py`).

### Phase 5: machines

- [x] ColdFire side, Digitakt II 1.16/1.17: fill reserved machine and param rows at run time (`core/dynreg`, `registry.rs`, `.dkm` `register`
  records, `patchkit pack --device`; docs/design/digikit-registry.md, finding 30). A data-only clone of SLICE (`test/dyn-xslice`) is in the SRC
  machine list after the load, is put on a track, and is gone after the unload; refusals write nothing; 0 stray writes, 0 flush hits.
- [x] ColdFire side, Digitone II 1.11/1.12 (finding 33): page rows (`pages`), `own_dsp`, the list getter filter (a hook at the `jsr (A3)` that ends the
  getter) and the view hook, a DN2 parameter row spec; `.dkm` PAGE records. `test/dyn-dn2` (own_dsp machine, four parameters, a SYN page
  with four knobs) is in the machine list after the load, goes on a track, its SYN page shows its knobs, it is gone after the unload; refusals write
  nothing; 0 stray writes, 0 flush hits (`pk_registry proof` and `ui` on dn2-1.11 and dn2-1.12). No audio: the machine renders silence until a DSP routine is loaded.
- [x] Unload refusal, both devices: `dkrt_unload` returns `E_TYPE_IN_USE` (-26) while a track of the active kit holds a machine of the module; accepted once the
  track has another machine (DT2 1.17 and DN2 1.12, headless and on the panel). Registry codes -20 to -26 are in `dkrt-sdk`. Open: the DSP selector word of a
  run-time machine on the Digitakt II; the other kits and the sound pool are not scanned.
- [ ] SHARC side, both devices: the runtime sends the DSP code in the frame tail (S4b); a static L2 stub at the
  block handler copies it into free L2, checks it, invalidates the instruction cache range and fills the voices
  registry. Optional faster load: the full-frame borrow.
  - [x] The channel: `core/dsp-link` (finding 31). ColdFire `dl_send`/`dl_poll`, a DSP stub at the end of the block
    handler, check, destination limits, cache invalidate (full, `FLUSH CACHE` and `ICAINV`; not modelled by the
    emulator), acknowledgement through the reply record. DT2 1.16/1.17 and DN2 1.11/1.12: 4 and 64 KiB byte exact,
    964 KB/s and 71.6 KB/s, PCM bit-identical to the idle image, +23/+26 DSP instructions per block.
  - [x] The runtime calls `dl_send` for a module's DSP object and fills the voices registry (finding 32): the `.dkm` DSP section
    (relocated on the ColdFire into a first-fit range of the 512 KiB / 320 KiB area), `core/dyndsp` (8 free voice rows, the table of
    DSP addresses), window 1 of `core/dsp-link` for one-word row writes, the load as a state machine in the UI-loop hook
    (`LOADING`, queue, unload order machine row, voice row, range). DT2: `test/dyn-pluck` loaded at run time gives PCM
    bit-identical to the patched-in `digikit/pluck` (`tools/patchkit/pk_dkrt_machine.py proof`).
  - [ ] Hardware: the cache sequence and the record offset `0x60` (finding 31, open points).
  - [ ] Optional: the full-frame borrow.
- [x] Port `digikit/pluck` through the SDK as the proof (ColdFire and DSP sides): `test/dyn-pluck` (typed sections + the pluck DSP source),
  Digitakt II 1.16 and 1.17. Digitone II: the DSP side only (`test/dyn-sine-dn2` against `test/dn2-sine`, type forced in the DSP's
  frame copy).
- [x] (done, lane r4, finding 39) Join the halves on the Digitone II: a `dyn-pluck` for DN2 (own_dsp machine and SYN page from finding 33, the DSP section from
  finding 32) loaded from the menu plays like the patched-in `digikit/pluck` on dn2-1.11/1.12; the Digitakt II proof through the menu as well
  (the proofs load through the mailbox today). This is the Phase 5 gate.
- [ ] The DSP selector word of a run-time machine (a clone of SLICE plays as SAMPLE; route and estimate in finding 32).
- [x] Projects that name a machine that is not loaded fall back to the gap row (the type byte stays; the row is a clone of type 0 with no
  knobs; the SRC page frame equals that of a never-filled gap row; finding 30).

Gate: a machine module selected in the menu plays in the emulator on both devices.

### Phase 6: boot-time loading (decision 9)

Hook (finding 36 proposed a `jsr` just before the MainScreenView constructor; finding 37 measured that the machine view already
exists there and moved it): a `hook` op at the **entry of the main-OS task** (DT2 0x400337ba, DN2 0x4002ed34). There the block read
works and nothing of the UI exists, so the rows are in place before the stock builder runs (no view hook, no rebuild at boot:
`view_rebuilds` 0). The frames that carry DSP code are not flowing at that point (a transfer started there is never acknowledged), so a
module with DSP code goes live in two steps: the CF part at the hook, the DSP part over the first UI-loop passes.

C ABI for the menu (names fixed here so the menu and the runtime can be built in parallel; layouts in `dkrt-sdk`):
- `dkrt_selection_get(out *mut DkrtSelection) -> i32`: the saved selection, which the next boot loads (store paths, at most 8).
- `dkrt_selection_set(sel *const DkrtSelection) -> i32`: write it (two copies with a sequence number and a CRC, in
  sectors of the store range that the +Drive builder keeps); no effect until the restart.
- `dkrt_restart() -> i32`: save the working state the way the firmware does (item below), then soft reset; returns
  only when it refuses (for example a card transfer in flight).
- `dkrt_boot_info(out *mut DkrtBootInfo)`: safe mode and why, the boot counter, each selected module's load result.
- `dkrt_status` and `dkrt_module` stay (what this session loaded).

Items:
- [x] Boot load: the hook, the selection record, loads in selection order through the existing checks (a module that
  fails is skipped and reported; the others load), DSP code through `core/dsp-link` (finding 37; `tools/patchkit/pk_dkrt_boot.py`).
- [x] Safe mode: a boot counter in the selection record, raised before the loads and cleared once the UI loop runs;
  after two boots that did not reach it, or with a key held at power-on, boot with no modules and say so. The counter and the
  record flag are proven in the emulator; the key is coded and **[O]** (no key reaches the emulator before the main OS runs).
- [~] Restart: `dkrt_restart` does the firmware's own soft-reset sequence (0x80 to 0xEC090000) and the emulator models the reset; before it, the
  firmware's save sequence (`FUN_40030c9c` serialize, then `FUN_400f044a` boot-record write; anchored on all four ports; writes only the firmware's own
  sectors, DT2 1.17 and DN2 1.12 run). **Not proven: that an edit survives.** Panel pattern and sound edits never reach the arena or a saved slot in the emulator
  (finding 40), so the menu must ask for SAVE PROJECT first; where such an edit lives is **[O]**.
- [ ] Menu: DIGIKIT > MODULES ticks the selection (greyed: other device or newer runtime), APPLY & RESTART, a line for
  safe mode, ABOUT with this session's modules and their results.
- [x] +Drive: the selection record round-trips with `plus_drive/<device>/digikit/selection.txt` (builder writes both copies, exporter writes the winner back; plusdrive tests and `plusdrive:check`; finding 40).
- [ ] Tests kept in the suite (routed by verify), one scenario format, from a booted snapshot where possible:
  - equivalence, any module type: booting with set S gives the same rows and tables at the main screen and the same PCM
    for a scripted project as the image patchkit builds with S patched in;
  - project round trip across sets: save under set A, boot with B, boot with A again: sounds, values, p-locks and
    sound locks byte-identical; a track whose machine is not loaded keeps its type;
  - safe mode: a module that faults in `init` twice leads to a boot with no modules;
  - power cut while the selection is written (reset between the two copies): the next boot uses a valid copy;
  - write-watch, flush check and card-store check on every scenario.
- [x] Archive commit (lane r4, 2026-10-17; list in docs/plans/digikit-hot-reload.md): remove the hot-only parts (unload, in-use scan, load queue, live LOAD/UNLOAD in the menu, the
  boot-unneeded view and rebuild hooks if the boot path proves them unneeded) in one commit, so `git revert` restores them.

Gate: on both devices, a set of modules chosen in the menu (a machine with DSP code, a code-only module) is loaded after
APPLY & RESTART, plays like the patched-in set, and the round-trip and safe-mode tests pass.
- [x] Menu gate (lane r4, finding 39): `uv run python -m tools.patchkit.pk_digikit_scen run menu-pluck-demo --tier full` (dt2-1.16/1.17, dn2-1.11/1.12: pass; ticks `test/dyn-demo` and `test/dyn-pluck`, APPLY & RESTART, PCM equal to the patched-in `digikit/pluck`, watch 0, flush 0, card writes only in the selection sectors). The Digitone II pluck is `test/dyn-pluck` (equiv: `run equiv-dyn-pluck --tier full`). Round trip and safe mode: `roundtrip-*`, `safe-fault` (finding 39).

### Phase 7: module upload and tools (2026-10-18)

Spec: docs/design/digikit-upload.md (framing, commands, store format version 2 with two header copies).

- [x] Device side in `core/runtime`: the SysEx hook in the MIDI task on all four ports, INFO/LIST/BEGIN/DATA/END/DELETE/CLEAR/ABORT,
  staging and the two-copy header commit; plusdrive builder, reader and exporter take store version 2 (lane r4).
- [x] A Python host tool, `dkupload` in tools/device (emulator and device), and the kept upload scenarios (lane r4).
- [x] Static check: can the runtime take its own SysEx before the firmware's per-event chain (finding 42 section 2, stages 2-3) to pass
  1.4 MB/s (lane r4, report only).
- [x] Emulator: the idle fast-forward recognises the core/health counting loop and credits the skipped iterations (lane b; DT2 cold boot
  with core/health 107 s -> 41 s, health readings identical; finding 41).
- [~] `devrpc` 8-in-7 framing fixed; finding 25's FsSample write check re-run with valid writes (lane b): all statuses 0, the store
  untouched; the written bytes never reach the card in the emulator (what flushes them is **[O]**, finding 25).
- [x] Desktop and web app: a Modules panel (list, upload, delete, repack) over this protocol (`packages/dkupload`), to the running
  emulator on desktop; the web page over Web MIDI to a device, tested against a mock only (the web emulator has no card); no device
  MIDI path on desktop yet (lane r1).

Gate: on both devices in the emulator, a module uploaded from the desktop app and from the web app appears in DIGIKIT > MODULES,
loads after APPLY & RESTART and plays like the card-built module; the cut and playback scenarios pass.
- [~] Gate status (2026-10-18): `menu-upload-pluck-demo` passes on all four ports (upload, MODULES, APPLY & RESTART, rows and PCM equal the
  patched-in set), with the upload sent by the Python tool; the panel's own code (`dkupload` `Session`) passes `upload-rust` on all four
  ports. Not yet run: the desktop panel clicked in the app, and the web page against a real or bridged device.

### Phase 8: hardening and device data (2026-10-18)

Safety (lane r4):
- [x] Safe mode also catches a module that crashes or hangs after the first UI pass: the boot counter is cleared only after a stable period (45,000 DSP blocks = 30 s of the firmware's block counter; a CPU exception halts the device; kept scenario `safe-late-fault`; finding 37, last section).
- [x] `patchkit pack` refuses a ColdFire object that has not passed the ISA check (the build records `toolchain.isa_check` over the object's bytes, pack verifies it; finding 43 "2.").
- [x] The menu's store reader cannot see a torn table while an upload commits (`store::open_stable`, fingerprint of the headers before and after, no lock; host tests that interleave a commit with every read; finding 43 "3.").
- [x] CLEAR cannot lose the store after a cut between header B and A followed by a torn header B (BEGIN makes the headers equal before staging; test `upload-clear-cut` in `modules/core/runtime/tests/upload-faults.device.test.ts`; finding 43 "4.").
- [x] A reply cannot hang the MIDI task when the host does not read (no unbounded spin on the TX ring): replies are dropped when the firmware's in-flight count says the ring has no room; kept scenario `upload-txfull`; finding 43 "5.".
- [x] HEALTH shows the module store's used and free space (`DkrtHealth` store fields, read from the headers once and again after an upload; finding 41, last section).

Device data (lane b): every device or board fact the runtime, the core modules, patchkit and port_resolve use comes from
`devices/*.toml` (or `devices/ports/*.toml`, `modules/core/machines/types.toml`) and is generated where code needs a constant
(`patchkit gen`, kept current by `gen --check`); the device list is open. Oracle: patched images are byte-identical before and after.
- [x] Store geometry, DSP area size, voice rows, run-time type range, device id and name, board addresses, safe-mode key, DSP clock. Where each fact lives and what a new device needs: docs/design/device-data.md. The runtime is one image for all devices, so a shared constant is refused when two devices differ.
- [x] patchkit's device list, device ids, container facts; port_resolve's firmware, limits and L2 tables. The container seal keys stay in `syx.rs` (key material; the device files are published as they are).

Testing ease (lane r1):
- [x] `mise run emu -- PATCHED.syx` recognises a patched image by its patch record (device, base firmware, +Drive card).
- [x] `patchkit pack` through the front end (and a mise task); docs/PATCHING.md: testing a run-time module in the emulator.
- [x] verify: no step runs a binary that a cargo build is rewriting (the `_dyld_start` hang of 2026-10-18): every `cargo build` replaces `release/<bin>` even when current; patchkit is now built under a lock and run from an immutable copy (tools/devtools/stablebin.py).

### Phase 9: Digitakt II on the device (2026-10-18)

Decisions (Em, 2026-10-18): Digitakt II first, end to end on the device; the Digitone II keeps its emulator tests and waits. Flash
through SETTINGS > SYSTEM > OS UPGRADE; recovery is the bootstrap's STARTUP menu over DIN (not rehearsed; finding 01 "Recovery").
The store location is per device in `devices/*.toml`; for the Digitakt II a folder in the sample filesystem is preferred if the spike clears it.

Emulator first:
- [x] Store spike (DT2) (report: finding 44; Em decides): a module folder in the sample filesystem: what the sample picker does with a `.dkm` (and a dot-folder), whether
  the filesystem is mounted at the boot hook, the write path for uploads (and why FsSample writes never reached the card, finding 25),
  where the boot record lives. Report, then Em decides.
- [x] Pluck on silicon (finding 45): the machine path no longer depends on any context register: `core/dsp-voice` (both devices) reads with
  immediate offsets and pins M5, M6, M7, M13, M14, M15, L0-L5 and MODE1 (saved, set, restored) around a routine; the rig draws hostile
  contexts and runs odd voices (117 of 118 tests fail on the old wrapper); `dl_diag` and `tools/patchkit/pk_ctxprobe.py` read the context at
  the hook. The emulator holds the constants and so does the stock renderer: the report's two causes do not by themselves explain the halt
  (finding 45, section 1). Open: read the real context and heartbeat on the device (rung C).
- [x] Card safety: a hard range check in the one card-write function; before the first write the runtime reads its area and writes only
  when it is blank or ours (else read-only, reported). Lane r4, commit 3a6bd772, finding 46, scenario `card-safety`. Open: the torn first write
  to a blank card.
- [x] Debug (toggle in DIGIKIT, on by default for now): a live log over the SysEx protocol, CPU exception capture across a soft reset,
  a DSP heartbeat watch, a read-only memory peek over ranges in the device TOML, and a Mac tool that writes the log to a file. Lane r4,
  commit 3a6bd772, finding 46, scenarios `debug-log` and `crash-capture`, `tools/device/dklog.py`, cost measurement `pk_digikit_cost` (PCM equal, +0.004 %
  instructions). Open: whether RAM survives a soft reset on the device (ABOUT `RAM KEPT` after one menu restart tells); the heartbeat watch has not run
  with a real `dl_diag` (lane b).
- [x] Preflight (`mise run preflight -- IMAGE`; rungs A, B, C defined in docs/PATCHING.md): before every flash, check the base is the known stock image, sections 2, 4, 5 and 8 are byte-identical, the checksum and
  HMAC hold, the sizes are in limits; print the hashes. Define the rung images.

Device rungs (stock 1.17 is the way back each time):
- [x] A: the runtime with no DSP change (section 3 only): boot, DIGIKIT, the log, the store-area check (read-only). Device, 2026-10-18:
  boots, log over USB, store range blank (finding 47).
- [x] B: with the DSP link, no modules: normal audio, the DSP heartbeat in the log. Device, 2026-10-18: heartbeats steady, the context
  registers at the hook equal the emulator's (finding 47).
- [x] C: upload pluck over SysEx, APPLY & RESTART, play, with the log capturing the DSP registers. Device, 2026-10-18/19: upload, boot load and restart
  work; the SHARC halted on the first note of the added type (rungs C, C2, C3a bisect it to the frame save); the cause was 17 stores the ISA excludes
  (DAG1 registers through I6); rung C3d2 with them through R2 plays the pluck (finding 47; finding 49 section 10). The legal saves are the default on both
  devices, and the build and preflight refuse the excluded stores (commit 8c7c7c93).
- [ ] C4: the default build (no variant options) plus one SRC page for added machines (finding 50): `out/patch/rungC4-dt2-1.17.syx`. Check on the device:
  pluck plays; SRC shows no page dots; SRC again, UP and DOWN do not change the page; SAMPLE/WERP/REPITCH keep their second page.
- [ ] The emulator refuses the excluded stores (a native trap at decode time in `sharc-rt`, both tiers; finding 49 section 10.7).
- [ ] D: debug off by default.

### Phase 10: a fast module loop and device debugging (2026-10-19; order set by Em)

Focus (Em, 2026-10-19): stability; machines and other module types working; fast build, test, upload and module testing with few flashes
(a module still needs select and restart to load); debugging on the device; device data that finds ColdFire and SHARC gaps in the emulator and
helps SHARC performance. Guardrails: everything behind DEBUG, read-only by default, never a card write outside the store or a flash write.
New tools approved (Em, 2026-10-19): `sccache`, `cargo-nextest`, a test-impact tool (pytest-testmon style).

1. [x] `dk run MODULE` (2026-10-19, lane dkrun: `tools/device/dkrun.py`, `mise run dk:run`, runtime 4 SELECT 0x30 and RESTART 0x31; emulator proof `menu-dkrun-pluck`; image `out/patch/rt4-dt2-1.17.syx`; device run: docs/PATCHING.md "Runtime 4: dk run and screenshots"): build, pack, upload, select, restart, follow the log, in one command. Runtime 4 adds SELECT (write the selection
   record) and RESTART (the soft reset the menu's APPLY & RESTART does) to the SysEx protocol; one flash, then no menu per change.
2. [x] Screenshot (2026-10-19, lane dkrun: `tools/device/dkscreen.py`; the emulator answers the stock Screenshot and the 1024 bytes equal its frame capture, scenario `upload-screenshot`; not yet run on the device): the firmware's own MidiRpc `Screenshot` (0x04, 1024 bytes of the OLED shadow buffer; docs/MIDI-SYSEX-RPC.md) through
   `tools/device/devrpc.py`, written as PNG; no flash. Check it on 1.17.
3. [ ] Verify speed (the agreed list; history and evidence in ~/src/digi/VERIFY-PERFORMANCE-PLAN-2026-10-14.md). Three lanes in parallel (2026-10-19):
   - [x] v-route (commit 632dc41b): finer routing in tools/devtools/verify.py (`mise.toml` only to the tasks it changes; `tools/device/*` to its tests;
     a module's files to `cargo test -p patchkit` and the scenarios that include X), routing tests proving nothing is dropped; mise split into
     `verify:pre` (lanes, 1-2 min), `verify` (routed; the merge gate), `verify:full` (generated-core changes); AGENTS.md and docs/TESTING.md.
     Example: the runtime-4 merge (one `mise.toml` line) ran all 13 steps, about 25 min, for about 4 min of relevant tests.
   - [x] v-rust (commit 632dc41b; nextest not installed, not used): Rust tests for affected crates only (reverse dependencies from `cargo metadata`, tools/devtools/rust_ws.py), `cargo-nextest`;
     `verify:full` keeps the whole workspace. rust-tests today 260-600 s.
   - [x] v-slow (merged; 417 s to 136 s CPU, tests_gen 121 s to 15 s): `tests/patchkit/test_pluck.py` sharded or cheapened (about 370 s alone; lever 2b, ceiling about 200 s); the SHARC `tests_gen`
     differential tests only when their crate changed (lever 2a, about 100 s).
   - [ ] Then, measured: `sccache` across lanes (only if it hits at the lanes' paths); why an emulator edit rebuilds so much (crate split).
   Baseline 2026-10-18: wall 881 s; python-tests 881 s, rust-tests 598 s, check-no-aot 492 s, tier-quick 421 s. Approved tools: `sccache`,
   `cargo-nextest`, a test-impact tool.
4. [ ] DSP debugging: DSP peek over the DSP link (read-only), a SHARC exception handler writing the faulting PC and status into the reply record,
   a PC sampler on a DSP timer (where a hang loops; a profile of the real chip for SHARC performance and the cycle model). Modules where possible.
5. [ ] Log levels (error, warn, info, debug, trace) per subsystem, set from the Mac at run time, besides DEBUG on/off; on-device timing of module routines.
6. [ ] Named dumps (address, length) in the device TOML, captured on the device and in the emulator at the same marker and diffed: a list of emulator gaps.

Deferred (Em, 2026-10-19): `dkflash` (Transfer takes about 20 s, then YES on the device after the data), the shared emulator/device interface,
key input, CPU tracepoints, the GDB bridge (docs/design/device-bridge.md holds the draft). Parked: an SDK-style module model in C or Rust (Machine,
Parameter, Page types) on top of the typed schema (finding 27) and `dkrt-sdk`. A second SRC page for a Digitakt II machine with more than eight
parameters needs engine slots, a descriptor per page and a typearg mapping (finding 50); today it is a build error.

### Phase 11: a module contract, a conformance suite and self-contained modules (2026-10-19; agreed by Em)

Why: on the device an LFO on pluck's TUNE silenced every track (log `out/runs/device-logs/lfo-crash.log`: the DSP kept running,
dispatches = returns). A sweep on the routine rig: pluck's loop period P = fs / f0 from note + TUNE is not clamped; outside the range
N can serve (about P < 2.3 or P > 4091.5) the allpass coefficient goes unstable and the string state holds Inf/NaN until the next note,
which reaches the send FX and stays there. In-range knob values reach it. Em, 2026-10-19: modules must be self-contained, the module's
tests test only the module, framework fixtures are not modules, the manifest holds metadata, a contract per module type is enforced
twice (a conformance suite before shipping, the runtime on the device), a template per type and one authoring guide.

1. [x] (lane b, finding 51; DT2 only, DN2 wrapper unguarded) Pluck's pitch clamped; the runtime guard in the voice wrapper: parameter words clamped to the declared ranges, output checked
   (non-finite or above a level bound: the block zeroed, the voice reset, counted; the module disabled after 8 faults in a second),
   `MACHINE_FAULT` in the log with the parameters. Emulator first, then one flash.
2. [x] (lane srcpage; the guard's fault count is an invariant, which found pluck's unwritten line) Machine conformance suite from the manifest on the routine rig (every parameter at its limits, notes x pitch parameters, per-block
   modulation, long notes, retriggers; finite, bounded, returns, within budget), `mise run module:check NAME`, routed by verify.
3. [ ] (folded into Phase 12) Layout: one module per machine (patched in or loaded at run time is a build choice; `test/dyn-pluck`, `test/dyn-xslice`, `twin-*`
   removed), framework fixtures from `modules/test/` to a fixtures folder under `tests/` (new), framework scenarios on fixtures, a module's own tests
   in its `tests/`. Oracle: images and `.dkm` byte-identical except the module ids.
4. [ ] (folded into Phase 12) Manifest with author concepts only: firmware codes (templates, knob codes, placement, entry) into the device profiles, parameter kinds.
5. [ ] (folded into Phase 12) `mise run module:new machine NAME` and docs/MODULE-AUTHORING.md rewritten around new, build, `module:check`, emulator, device.
6. [ ] `Target` (emulator and device backends over MIDI, our SysEx, the firmware screenshot, audio capture) and system scenarios
   (`module:emu`, `module:device`); FX sends, LFO on every parameter, machine and track changes.

### Phase 12: the module SDK (design: docs/design/module-sdk.md; agreed to build, 2026-10-19)

Em, 2026-10-19: generic modules per type with one contract and entry point; one adapter per device driven by the device profile;
the runtime tracks what is loaded and where, prevents clobbering and enforces limits; checks at build time where possible; readable
for humans and LLMs; hot reload kept possible (not built); logging with levels from emulator and device alike. Pluck first, other
modules untouched until pluck is migrated and tested on the device; then FX and the other types.

1. [x] The design's open questions answered (module-sdk.md section 11.4).
2. [ ] Migration steps of section 10.1, pluck first, each with its oracle (section 10.2: rig samples and scenario PCM equal to today's,
   conformance and guard tests pass, stock pins unchanged); Phase 11 items 3-5 are part of this.
3. [ ] Pluck on the device through the SDK (one flash for the new runtime, then `dk run`).
4. [ ] The FX contract with the first FX module; then the other types.

### Later

- Projects and modules: whatever a project persists that names a module (machine types, FX, ...; to check) is matched against the
  loaded modules on project load: report a missing or different module, later offer to load it (Em, 2026-10-19).

- Hot loading and unloading: docs/plans/digikit-hot-reload.md (archived state and what is left).
- Samples over a vendor bulk interface (finding 42 route B), after a device measurement of the card write rate; mass storage only if that fails.
- Faster transfer: assess separately. USB mass storage would show Elektron's filesystem, which no host can mount,
  so it would need a FAT view on the device or a host tool.
- Browser: the web emulator has no card today; the dev loop is desktop only.
- Existing modules to Rust: only when a module gains from it (for example to load at run time).
- A DSP intermediate language: voices built from blocks (oscillator, filter, envelope) and lowered to SHARC
  assembly. Assess after two machines are written by hand through the SDK.

## Payoff, confidence, risks

- Phases 1 to 4 remove the re-flash for each module set and make modules portable across firmware versions.
  Confidence: high for 1 to 3, medium for 4 (menu reuse not proven).
- Phase 5: run-time machines on both devices through the frame tail (S4b: 8 to 10 agent hours for the channel,
  about 70 % confidence).
- Risks: the patched Rust toolchain to maintain (S1b); menu and text drawing (S2); hardware cache behaviour that the
  emulator does not show (S5 detector); firmware paths that write the arena and are not yet exercised (phase 2);
  hardware never ran `core/expansion` (no flashing until the emulator work is complete).
