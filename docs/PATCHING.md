# Patching the Digitakt II and Digitone II firmware with patchkit

patchkit applies modules (new machines, parameters, master-bus effects) to a stock Elektron `.syx` and writes a new
`.syx` that the device's updater accepts. A module holds our own code and a manifest. It never holds firmware
bytes. The tool runs on the command line (`mise run patch`) and in the browser (the `/patch` page).

**Patched images from this repository were flashed to one Digitakt II (OS 1.17) only, rungs A to C4 in October 2026
([finding 47](findings/47-dt2-device-rungs.md)); no Digitone II has run one.** Every other result here comes from the emulator.
Read [Safety and hardware](#safety-and-hardware) before you flash anything.

New to modules? Start with [MODULE-AUTHORING.md](MODULE-AUTHORING.md): the short path from an empty module to a
patched firmware running in the emulator.

Supported firmware (a "port" exists for each): Digitakt II 1.17 and Digitone II 1.12, and Digitone II 1.11 as a secondary port (only the modules whose manifest lists `dn2-1.11` in `ports` and `[targets]`: `core/expansion`, `core/runtime`, `core/health`). Use your own
copy of the `.syx` files; the repository never contains them.

Other documents: [modules/README.md](../modules/README.md) (write a module),
[docs/plans/patchkit.md](plans/patchkit.md) (plan and state),
[findings 21](findings/21-patchkit-core.md) (core), [01](findings/01-container-and-patching.md) (container, recovery,
flash map, expansion region), [20](findings/20-patch-toolchains.md) (toolchains). The notes from before patchkit
(hand-written byte patches on 1.15C and 1.10E) are in [archive/PATCHING-pre-patchkit.md](archive/PATCHING-pre-patchkit.md).

## Quick usage

Run every command from the repository root.

### Prerequisites

```sh
mise install
mise run setup        # Python and web dependencies
mise run patch:toolchains   # pinned m68k-elf gcc/binutils and the public SHARC+ assembler, into out/tools (about 15 min)
```

`mise run patch:toolchains` is needed only to build module sources (`module:build`, `module:new`, `module:dev`). Applying
modules that are already built needs no toolchain. Put your firmware files in `firmware/`
(`Digitakt_II_OS1.17.syx`, `Digitone_II_OS1.12.syx`, ...). `uv run python -m tools.patchkit.pk_toolchains status` prints the
installed versions.

### Patch a firmware

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/fx -o out/patch/fx.syx
```

This writes `out/patch/fx.syx` and `out/patch/fx.plan.json` (every write with its owner, diffable). Without `-o` the
name is `out/patch/<firmware>+<modules>.syx`. The command verifies the result before it reports success. Notes:

- Without `--modules` the device's patch set is applied: `device` by default, `--set NAME` for another (`devices/*.toml`
  `[patch_sets]`, roots only). `device` and `emulator` are both the flash-image modules of the rt021 images for now
  (dynreg, dyndsp, digikit-menu and health, plus what they require); `emulator` is what the emulator patches in itself
  (below), and will gain modules that a device does not take.
- `--modules` is a comma-separated list of module ids (their path under `modules/`). The command line adds the
  modules they `requires`, like the web gallery, and says so (`added by requires: core/machines, ...`). A module that
  is not in the library, a missing requirement and a `requires` cycle are refused.
- Modules apply in a fixed order (dependencies first, then by id), whatever order you write them in.
- The input must be a stock image. A patched image is refused.
- Everything under `out/` is derived from firmware and is never committed.

A machine needs its framework modules, which are added for you (`test/patched-pluck` is the patched-in test machine; the project's PLUCK, `digikit/pluck`, is a run-time module, see below):

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules test/patched-pluck -o out/patch/pluck.syx
```

### Inspect, check, explain, verify

```sh
uv run python -m tools.patchkit.patchkit list                                   # modules and ports
uv run python -m tools.patchkit.patchkit inspect out/patch/fx.syx               # device, sha256, port, patch record, sections
uv run python -m tools.patchkit.patchkit check --firmware firmware/Digitakt_II_OS1.17.syx           # which modules fit this firmware
uv run python -m tools.patchkit.patchkit verify out/patch/fx.syx --base firmware/Digitakt_II_OS1.17.syx   # re-decode and check
mise run patch:explain -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/fx
mise run patch:mem -- core/machines test/patched-pluck --device dt2 --firmware 1.17    # bytes used and free per region
```

- `check` prints `ok` or `refused` plus the reason for every module (for example "built for dn2-1.12").
- `patch:mem` (`tools/patchkit/pk_mem.py`, `--json` for scripts) applies the modules with the placement's own first-fit
  allocator and prints, per region of the port (`cf.flash_cave`, `cf.expansion`, `dsp.l2`), the size, the bytes each
  module uses (modules pulled in by `requires` included), the free bytes and the percent used. It exits 1 with the
  placement's message when the set does not fit. Without modules it lists the port's free regions and needs no firmware.
  `pk_harness run` writes the same readout as `memory` into `out/runs/patchkit/dsp/CELL.json`.
- `verify` re-decodes the image and checks the container (packet checksums, content checksum, HMAC trailer), the size
  limits and the patch record. With `--base` it also checks that sections 2, 4 and 8 equal the stock image.
- `explain` lists the allocation map (what went where, per region), each patched site with the bytes before and
  after, and the SHARC+ listing of DSP sites. Its output contains disassembly of firmware code: keep it out of
  commits.

### Run it in the emulator

```sh
mise run workbench            # desktop emulator, or:
mise run emu:serve -- ...     # headless daemon on the control socket (see below)
mise run patch:inject -- out/patch/fx.syx
mise run patch:inject -- out/patch/fx.syx --hot
```

`inject` loads the image into the running emulator through the control socket (`firmware.load`). Without an
argument it takes the newest image under `out/patch/`. The report says how the image started:

| `start_used` | Meaning | Time (DT2 1.17) |
|---|---|---|
| `hot-dsp` | Only the DSP section changed: DSP memory is patched while the firmware runs | about 1 s |
| `snapshot-cache` | MAIN OS changed and this MAIN image was booted before: the saved ColdFire state is restored | under 1 s |
| `cold` | MAIN OS changed: a coupled cold boot with DSP audio, then cached by the MAIN hash | about 15 s (DN2 about 6 s) |

`--hot` passes `--start snapshot`: it never cold boots and fails when no saved state fits.

`mise run patch:inject -- firmware/Digitakt_II_OS1.17.syx --runtime` (or `mise run emu:ctl -- load STOCK.syx --runtime`) lets the
emulator patch the stock image itself: `firmware.load {runtime: true}` applies the `emulator` patch set in-process and caches the
image in `out/runtime-cache/` (about 1.3 s the first time, 0.08 s after). `--patch-set NAME` picks another set. An image that already
has a patch record is loaded as it is. Details: docs/CONTROL.md.

Headless daemon with DSP audio (the flags `tools/control/digi_client.py` uses for its own daemons):

```sh
R=out/ready-pk-dt2-117
mise run emu:serve -- --auto-coupled --audio-profile-syx firmware/Digitakt_II_OS1.17.syx \
  --dsp-image $R/digi-audio-dn2-image.bin --dsp-state $R/digi-audio-m5.snap.dsp \
  --cf-snapshot $R/digi-audio-m5.snap --no-audio firmware/Digitakt_II_OS1.17.syx
mise run emu:ctl status       # from another shell; `mise run emu:ctl quit` stops the daemon
```

The ready inputs come from `uv run python -m tools.emulator.ready dt2 --syx firmware/Digitakt_II_OS1.17.syx --out out/ready-pk-dt2-117
--force` (about 2 minutes). The test harness looks for them in the table at the top of `tools/patchkit/pk_harness.py`;
`DIGI_PK_READY_<DEV>_<VER>` (for example `DIGI_PK_READY_DT2_117`) overrides the directory. The harness refuses ready
inputs that are stale or belong to another `.syx`.

### Test a run-time module in the emulator

A run-time module (`.dkm`) is loaded by the digikit runtime from the +Drive card, so the emulator needs the runtime set
in the firmware and a card.

```sh
uv run python -m tools.patchkit.patchkit patch --firmware firmware/Digitakt_II_OS1.17.syx \
  --set emulator -o out/patch/rt-dt2.syx          # the same as --modules core/dynreg,core/dyndsp,core/digikit-menu,core/health
mise run module:pack -- digikit/pluck --device dt2       # out/patch/pluck-dt2.dkm (--device dn2 for the Digitone II)
mkdir -p plus_drive/dt2 && cp notes.txt plus_drive/dt2/    # any file: the card is built only when the folder has content
mise run workbench -- out/patch/rt-dt2.syx
```

`mise run workbench` reads the patch record of the image (`DKPR`, through `patchkit inspect`), so a patched image gets the
device, the stock base image for the card builder and the `plus_drive/<device>/` card, the same card that `inject`
keeps. Then:

1. Upload the module: the Transfer page of the desktop app, or
   `uv run python -m tools.device.dkupload --emu put out/patch/pluck-dt2.dkm` (it lands in `modules/` of the store).
2. On the emulated device: SETTINGS > DIGIKIT > MODULES, select the module, APPLY & RESTART.
3. After the restart check the module in HEALTH and ABOUT (the same menu).
4. Quit the emulator: the card's files, including the module store (`digikit/`), are exported back to
   `plus_drive/<device>/` (`-- --no-sync` turns that off). The next start loads them again.

Use `DIGI_PLUS_DRIVE=DIR` or `-- --plus-drive DIR` for a scratch folder instead of the repository's `plus_drive/`.

### Remote control and a device on a MIDI port

The desktop app has a target selector in the Transfer and Remote pages: the emulator, or a MIDI port (the app opens it itself, through the Rust crate
midir; it starts no process). The Transfer page then lists, installs and deletes on the device. The Remote page shows the target's own screen (the
firmware's Screenshot, polled) on the faceplate, and its keys and knobs press and turn on the target through `digikit/remote`, a run-time module:
install it on the Transfer page (`out/modules/<device>/digikit/remote.dkm` after `mise run module:pack-all`), select it in MODULES on the device, apply and
restart. The commands work while the module is loaded; its check mark in MODULES is the only switch, and its SETTINGS... page (in the MODULES side panel) shows the held keys and
has RELEASE ALL KEYS (`modules/tools/remote/README.md`). From a terminal: `uv run python -m tools.device.dkupload --emu remote state|key CODE|turn ENC N`
(`--port NAME` for a device).

### Developer loop

```sh
mise run module:dev -- --modules core/fx [--device dt2|dn2] [--firmware 1.17]
mise run module:dev -- --modules core/fx --once      # one iteration, then exit
```

One daemon stays up on the stock firmware. Each time a file under the module's `src/` or its `module.toml` changes,
the loop builds the objects, patches from the stock image, loads the image into the daemon and runs the module's quick
tests. It prints one line per edit:

```
05:35:22 OK   build 0.0 s  patch 0.5 s  load 0.9 s (snapshot)  tests 1/1 2.2 s   edit->running 1.4 s  edit->tested 3.6 s
```

Measured on DT2 1.17: a DSP-only edit is running in about 3 s, a ColdFire edit in about 13 s (cold boot, cached by
MAIN hash). The daemon takes 1 to 2 minutes to start.

### Tests

```sh
mise run patch:test                                                    # the whole matrix (about 1 minute per cell)
mise run patch:test -- --cell "dt2-1.17:core/fx,test/cf-demo"   # one cell; modules joined with a comma
mise run patch:test -- --cell "..." --update                           # record frame goldens for a new module set
uv run python -m tools.patchkit.pk_harness list                                 # the cells of tests/patchkit/smoke/matrix.toml
cargo test -p patchkit                                                 # the patching library
uv run python -m pytest tests/patchkit/test_patchkit.py tests/patchkit/test_pk_prims.py  # codec, linker, primitives (about 2 minutes)
uv run python -m pytest tests/patchkit/smoke --slow                    # the matrix through pytest
```

A matrix cell is device x firmware x module set. For each cell the harness patches, loads the image into a daemon and
runs the smoke tour (boot, track and pattern change, audition, play and stop, machine menu, save and reload a project,
a seeded 40-input stress) plus the `tests/*.toml` files of the modules in the set. After every input it asserts no
fault, no DSP halt and no main-loop stall. Frames are compared by SHA-256 against
`tests/patchkit/smoke/goldens.json`, keyed by device, firmware and module set (hashes only). A module set with no
recorded goldens fails with "no golden"; record them with `--update`. A frame that differs saves `got.png`, `want.png`
and `diff.png` under `out/runs/patchkit/frames/<cell>/mismatch/` and prints the changed box and pixel count; `want.png` comes
from the last passing frame of that golden, kept locally in `out/cache/smoke-frames/` (filled by a passing run or `--update`,
never committed). `run --goldens FILE` compares against a copy of the goldens. Our own screens are better checked by the state they
are drawn from: `expect_text` (rows a module keeps in RAM) and `expect_mem` with `symbol` (a symbol of the image's plan), see
`modules/core/apps/digikit-menu/tests/menu.toml`. Details: the docstring of `tools/patchkit/pk_harness.py`
and [finding 17](findings/17-emulator-control.md). The proof tools `tools/patchkit/pk_proof_*.py` (PCM and behaviour proofs per
module family) are slower and are run before a merge, not in the loop.

### Create a module

```sh
mise run module:new -- machine alice/drone     # a run-time machine
mise run module:new -- fx alice/crunch         # a run-time effect
mise run module:new -- tool alice/hello     # a ColdFire service in C
mise run module:build                         # rebuild all objects (`-- modules/fx/my-effect` for one, `-- --check` only checks)
```

The command writes `modules/<category>/<name>/` (manifest, source stubs, `tests/smoke.toml`, `README.md`, `gallery/icon.svg`) and
builds it, so the template is known to compile. [modules/README.md](../modules/README.md) is the authoring guide and
the manifest reference.

### Resolve a port for a firmware

```sh
mise run patch:port -- firmware/Digitakt_II_OS1.17.syx   # one firmware -> devices/ports/dt2-1.17.toml
mise run patch:port                             # the supported firmwares (DT2 1.17, DN2 1.12, DN2 1.11)
```

A port records the anchors that modules need (address, length, SHA-256 of the bytes there) and the free regions of
one stock image. It is generated from the rules declared in `devices/fw/` and is deterministic: resolving a
supported firmware again leaves `git status` clean. A rule that matches nothing, or more than one place, is an error;
that is how a new firmware shows which rules need work. A firmware without a port file has no compatible modules.

### Web gallery

```sh
pnpm dev              # builds the browser core and patchkit.wasm, generates the module bundle, serves http://localhost:4321 (home; the emulator is /emulator/, patching and flashing /firmware/, modules /transfer/)
```

Open `/firmware/`. Choose your own stock `.syx` (it is read in the browser and never uploaded): the page patches the runtime set
in, checks the image and offers it as a download, flashes it over Web MIDI, or restores the stock file. "Run in emulator"
starts the patched image in the browser core (screen and controls, no DSP audio yet). Machines and effects are installed over USB on
`/transfer/`. The page reads module.toml at build time. `patchkit.wasm` reads
`public/modules/bundle.json`, which `apps/web/scripts/gen-modules.mjs` generates from the committed `modules/` and
`devices/ports/` (never from `out/`). `patchkit.wasm` produces the same bytes as the command line (`apps/web/tests/patchkit.test.mjs`). The
first `pnpm dev` in a fresh tree compiles the browser cores and takes several minutes.

## How it works

### Layers

| Layer | Holds | Examples |
|---|---|---|
| Port | Per-firmware facts: named anchors with SHA-256 of the expected bytes, free regions, container limits. Generated, keyed by the `.syx` SHA-256. | `devices/ports/dt2-1.17.toml` |
| Framework module | Hooks firmware sites once and exports a registry that other modules add entries to. Ordinary assembly plus a manifest. | `core/machines`, `core/params`, `core/dsp-voice`, `core/expansion` |
| Content module | Own code and declarative registry entries. No addresses. | `test/patched-pluck` |

Device, board and firmware facts are data (`devices/*.toml`, `devices/ports/*.toml`, `modules/core/adapters/machines/types.toml`); code that needs a constant gets it from `patchkit gen` (`[[gen]]` in a `module.toml`). Where each fact lives and what a new device needs: `docs/design/device-data.md`.

patchkit itself (`packages/patchkit`, Rust) has no framework-specific code. It provides generic primitives, and the
frameworks are modules built on them. One implementation serves the command line and `patchkit.wasm`.

### Ports and anchors

Modules never contain addresses. A firmware site is a named **anchor** (`dsp.master.site.q31_converter.call`,
`cf.machine.fn.dispatch`). A rule in `devices/fw/` ([design](design/firmware-decls.md)) declares how to find it, and
`patch:port` turns the rules into the lockfile. Rule kinds:

- SHARC+: `sharc_func` (a function found by its relocation-insensitive hash, plus an offset), `sharc_literal`,
  `sharc_mem_addr`, `sharc_table`.
- ColdFire: `cf_func` (a function found by a hash that masks address operands, so one rule serves relinked builds,
  plus an offset), `cf_ptr`, `cf_words` (a masked word signature), `cf_params_t1`, `cf_params_refs` and
  `cf_params_bounds` (many anchors from one rule), `cf_reset_init_calls`, `cf_reset_helper`.

The lockfile stores, per anchor, the address, the length and the SHA-256 of the bytes, never the bytes. At patch time
patchkit checks every anchor a module uses against the user's image; a mismatch refuses the patch. This is why one
module works on 1.16 and 1.17 although the 1.17 ColdFire image is relinked and every address moves. The full syntax of
the rules is the docstring of `tools/patchkit/port_resolve.py`.

### Module layout and manifest

```
modules/<category>/<name>/
  module.toml          manifest
  src/cf/*.c|*.s       ColdFire sources (m68k-elf gcc, -mcpu=54418, freestanding)
  src/dsp/*.s          SHARC+ sources (algebraic syntax)
  build/*.dko.json     built objects: committed for core/* modules; any other module has them in out/cache/dko (build output)
  tests/*.toml         harness tests
  README.md            notes: what it does, devices, limits
  gallery/icon.svg     the gallery card's icon
```

The manifest has `[module]`, `[targets]`, `[[object]]`, `[[op]]`, `[[claim]]`, `[[section]]`,
`[[registry]]` (with `[[registry.field]]`) and `[[entry]]`. Every key is listed in
[modules/README.md](../modules/README.md).

### Primitives

- **Objects and placement.** An `[[object]]` is a relocatable object built from our sources and placed in a region
  (`dsp.l2`, `cf.expansion`, `cf.flash_cave`). patchkit assigns addresses first-fit and applies the relocations.
- **Writes.** `write` replaces bytes at an anchor plus an offset. It carries `expect_sha256` of the bytes it replaces
  (never the bytes), or a typed `value` with a width of 1, 2 or 4 bytes and a range check. `anchors = "PREFIX.*"`
  repeats one write over many anchors.
- **Hooks.** A hook is declared in `devices/fw/` (site, `mode`, register contract). A `hook` op names it
  (`hook = "<hook name>"`, `to = "<symbol>"`). Mode `before` copies the displaced instructions to a tail
  (`hook.<site>[.<offset>].tail`; PC-relative forms are re-encoded or refused) so that the routine can run them and
  return (`.return`). Mode `replace` drops them. Mode `retarget` rewrites the target of an existing call.
- **Sections copied from the image.** `[[section]]` copies bytes of the user's image at patch time (a stock table),
  may append one row per registry entry, and places the result. This is how a framework extends a firmware table
  without storing it.
- **Registries and entries.** A framework declares a `[[registry]]` with typed fields. A content module adds
  `[[entry]]` rows. patchkit validates the fields, assigns indices in canonical module order, builds the row table and
  strings in the registry's region, and defines the symbols `<reg>.first`, `.count`, `.rows`, `.row_size` and
  `<reg>.<id>.index`. Every entry claims `<reg>.<id>`.
- **Expressions.** Wherever a number is accepted: integers, symbols, `+ - * / & | << >>`, parentheses, and
  `image.u8|u16|u32(anchor + offset)`, a checked read from the user's image (for example a stock bound).
- **Device filters.** `devices = ["dt2"]` on objects, sections, ops, entries, claims, rules and registries lets one
  module hold Digitakt II and Digitone II sources side by side.

### Regions

| Region | Where | Size | Use |
|---|---|---|---|
| `cf.flash_cave` | zero padding at the end of the MAIN OS image | about 2 KB | the 54-byte expansion loader only |
| `cf.expansion` | RAM above the DMA arena: DT2 `0x47e8b000-0x47fe0000`, DN2 1.12 `0x4671a000-0x47fe0000` | 1.33 MiB (DT2), 24.8 MiB (DN2) | module ColdFire code, data, bss, registry tables |
| `dsp.l2` | free SHARC+ L2 behind the loaded stream | 914,564 B (DT2), 466,372 B (DN2), below the cache range at 0x200fa000 (finding 100) | module DSP code and data |

**Expansion segment and loader.** ColdFire code does not fit in the image, so patchkit appends the module payloads
to the MAIN OS image as a chain `[len][dest][bss][payload] ... [0xffffffff]` and adds the built-in `core/expansion`
module whenever a module places something in `cf.expansion`. Its loader (in the flash cave) replaces the reset code's
two helper calls (copy, clear). It copies each payload to its destination, zeroes the module's bss, then calls the two
original helpers. Module code runs in place before the firmware's own initialisation, so it must not assume a libc.
The region is above the bss end, so the reset clear does not touch it, and a canary run found no firmware access
([finding 01](findings/01-container-and-patching.md), "Expansion region").

**DSP boot-stream append.** SHARC+ code is added as new blocks at the end of the boot stream in section 7, placed in
free L2. The ColdFire boot routine sends the whole decompressed section, so no ColdFire change is needed. Limits:
stored size at most `0x100000`, decompressed size at most `0xfffff`; usable appended payload 727,540 B (DT2) and
211,600 B (DN2). patchkit checks every output stream against the boot-ROM rules that the public manual states
(`packages/patchkit/src/bootrom.rs`, [finding 06](findings/06-sharc-engine-and-startup.md), "Boot ROM conformance").

### Linking and `.dko` objects

`patchkit build` (`packages/patchkit/src/build/`) compiles a module's sources with the pinned toolchain and writes a `.dko.json` object in
canonical JSON: sections as hex, symbols, relocations with explicit addends (`cf.32`, `cf.pc32`, `cf.pc16`,
`cf.pc8`, `sharc.abs24`, `sharc.abs32`), the source hash and the toolchain version. Our own decoders re-decode every
instruction at build time, so a toolchain encoding error cannot reach an image. At patch time patchkit links the
objects. Undefined symbols resolve to anchors (`[object.externs]`), to symbols of other modules, or to symbols that
patchkit defines (registries, sections, hook tails). Objects are committed, so patching and the browser need no
toolchain.

### Conflicts, claims and order

- A `[[claim]]` names a scarce resource (`dsp.master.hook`, `cf.machines.sites`). Two modules that claim the same one
  are refused. Registry entries claim `<registry>.<id>` automatically.
- `requires` must be selected; `conflicts` must not be. `ports = [...]` in `[module]` pins a module to firmware.
- patchkit refuses overlapping byte ranges after linking, a region over capacity and an anchor hash mismatch.
- Modules apply in canonical order: dependencies first (topological), then by id. The same module set gives the same
  `.syx` bytes whatever order you list it in.

### The patch record

patchkit stores a `DKPR` record in the image: format version, the SHA-256 of the stock `.syx` and the list of
`id@version`. It lives in `cf.expansion` when the patch uses it, otherwise in an appended DSP block. `inspect` and
`verify` read it, and a patched image is never patched again. Nothing on the device reads it.

### Codec and safety rules

- The codec reproduces all four stock images byte for byte (framing, 8-in-7 coding, packet checksums, content
  checksum, HMAC trailer). Sections that a patch does not change keep their stored bytes. Changed sections are packed
  with an aPLib packer that produces the same bytes as `tools/firmware/aplibpack.py` and runs about 25 times faster.
- **Sections 2 (bootstrap), 4 (updater) and 8 never change.** patchkit has no override. The bootstrap is what lets you
  recover from a bad MAIN OS.
- The container is at most `0x300000` (3 MiB). The OS container is written to flash at `0x80000`; the record sectors
  (tagged fields, serial number) start at `0x380000`, and the bootstrap's write loop has no size limit of its own.
  Stock images are about 1.5 MB.
- patchkit re-decodes and verifies every output before it reports success: checksums, HMAC, size limits, the patch
  record, and (with a base) that sections 2, 4 and 8 equal the stock image.
- Never flash the output of `tools.firmware.build.rebuild`: it stores sections without compression and exceeds the cap
  ([finding 01](findings/01-container-and-patching.md)).

### What is never stored in the repository

Firmware (`*.syx`), sections extracted from it, snapshots, ready states, card images, patched images, `plan.json` and
`explain` output of a real patch, databases derived from firmware, and users' sample audio. Ports hold addresses and
SHA-256 values. Modules hold our code, our objects and SHA-256 of the bytes they replace. Frame goldens are SHA-256
only. Modules that need firmware words (oracles) are generated under `out/modules/` and read through `--modules-dir`
or `PATCHKIT_MODULES`. The web page builds its module data from committed files only, and the build guard rejects
`.syx`, `.snap` and `.mstate` files.

## Safety and hardware

### What the emulator has shown

The bootstrap (section 2) runs on the emulated ColdFire core. It tests [FUNC] before MAIN OS is touched, shows the
STARTUP menu over a MAIN OS that faults at reset, accepts patchkit images, rejects corrupt ones, and does not rewrite
itself for a 1.16 or 1.17 image (finding 01, spike 0.2). Patched images boot to the main screen with DSP audio
(DT2 1.17, DN2 1.12), the smoke matrix passes, and the module proofs measure the sound. None of this is a
hardware result.

### Recovery

- Hold **[FUNC]** while powering on. The STARTUP menu comes up from the bootstrap, which patchkit never changes.
- In the STARTUP menu, **[TRIG 4]** selects OS UPGRADE. It receives over **MIDI DIN only**; USB MIDI is not available
  there. Have a MIDI DIN interface ready.
- The bootstrap checks only the content checksum and the HMAC. On failure it prints UPGRADE ABORTED and writes nothing.
  That the menu comes up with a broken MAIN OS is shown in the emulator, not on hardware.
- **Keep the stock `.syx` of the version on the device.** Recovery is installing it again.
- Do not save over a stock project while testing, and back up the card. A project saved with an added machine loads on
  stock firmware with that track as SAMPLE.

### Pre-flash review

The full table of 14 risks, with evidence and mitigation, is in [docs/plans/patchkit.md](plans/patchkit.md)
("Pre-flash review"). The ones that matter most:

1. **The real SHARC boot ROM may reject a patched DSP stream** (it could apply a rule the manual does not state). A
   rejected stream is a boot hang: the ColdFire waits for the DSP with no timeout. Flash in this order: the harmless
   image that changes nothing (`tools/patchkit/patched_image.py`), then a DSP change without an appended block, then an
   appended block. Keep the stock `.syx` and the recovery steps at hand.
2. **Cycle cost.** The emulator counts instructions, silicon counts cycles. Keep a module small (at most a quarter of
   the idle instructions per block) and listen for clicks at the first run.
3. **Digitone II patches cannot be tested on hardware** (no device available). They are emulator-tested only.
4. **Level.** A gain or drive module can raise the output. Start at the neutral setting with the monitor volume low.
5. The MAIN OS image validator and the real flash writer were run in the emulator only (finding 75); the emulated flash has no erase time and the flash writer's return codes are ignored by the firmware. The bootstrap's own checks were run too.

### Flashing a Digitakt II, rung by rung

Phase 9 of `docs/plans/digikit-runtime.md`. Each rung is a patched image of the stock 1.17 firmware that adds one kind of risk to
the one before. Flash one rung, look at the checks, and only then take the next. Stock `firmware/Digitakt_II_OS1.17.syx` is the way
back from every rung. Rungs A to C4 were flashed on Em's device (finding 47); the module sets below were built and checked by
`mise run patch:preflight` on 2026-10-18 (lane r1), and the device steps are the plan they followed.

**Before each flash** (the patch needs the stock file in `firmware/` and the module objects built, `mise run module:build`):

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules RUNG_MODULES -o out/patch/dt2-1.17-rungX.syx
mise run patch:preflight -- out/patch/dt2-1.17-rungX.syx            # must end with "PREFLIGHT OK"; exit 1 and "PREFLIGHT FAILED" otherwise
mise run workbench -- out/patch/dt2-1.17-rungX.syx                  # optional: the same image in the emulator
```

`preflight` checks: the patch record names a base that is a `[[firmware]]` of `devices/*.toml` and the stock file with that sha256 is
in `firmware/`; sections 2 (bootstrap), 4 (updater), 5 (meta) and 8 (boot data), and any other section but 3 and 7, are byte-identical
to the base (stored and decoded; a difference fails); which of 3 (MAIN OS) and 7 (DSP) differ; the container checksum, the packet
checksums, the HMAC and the section decode (`verify`); the size limits of the port, printed as used of maximum; and the sha256 of the
patched image and of the base. A stock image is refused as "not patched". Compare the printed sha256 with the file you copy to the
device.

| Rung | What it adds | `--modules` (requires are added automatically) | Sections that change | Image bytes |
| --- | --- | --- | --- | --- |
| A | the runtime and the menu, no DSP change | `core/runtime,core/digikit-menu,core/health` | 3 only; 7 is the stock DSP | 1,869,472 |
| B | A plus the DSP link and the run-time machine rows (no module installed) | `core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp` | 3 and 7 | 1,876,512 |
| C | B's module list rebuilt (the DSP now writes breadcrumbs, the runtime decodes them: a new image, flash it), then the test module `digikit/pluck` uploaded at run time | same as B (the module is a file, not part of the image) | 3 and 7 | 1,885,088 |

- Rung A: the requires added are `core/expansion` only. `core/health` has no DSP part: it reads the DSP's own cycle count from the reply
  record and counts idle iterations on the ColdFire, so it needs no `dsp-link` and leaves section 7 stock (checked: the plan changes
  section 3 only). A module with a DSP part cannot load on rung A. Its HEALTH page is the first thing to compare with the emulator.
- Rung B: the requires added are `core/machines`, `core/params`, `core/dsp-link` and `core/dsp-voice`. The DSP section changes (a stub in
  the block handler, finding 31, and the voice rows). `core/dsp-voice` carries the open silicon question of the pluck voice
  (M6/M13, `docs/plans/digikit-runtime.md` Phase 9 "Pluck on silicon"): the image plays stock sounds normally in the emulator, but
  nothing about the odd-voice copy has run on a device. Rung B is the first rung where the DSP boot ROM reads a changed stream (pre-flash
  review item 1 above).
- Rung C: the module list of rung B, built again (the DSP wrapper and the debug log changed since rung B, finding 48), plus the test module
  uploaded over USB at run time. The whole procedure is the section "Rung C" below.

**Flashing with `patchkit flash`** (built 2026-10-28, RPC route 2026-10-29; proof in the emulator only, [finding 75](findings/75-os-upgrade-receive.md)). One command
patches the stock file with the device's patch set, checks the image and the exact messages it will send the way the device's receiver does
(checksums, HMAC, the DT2 build floor, the product byte, the container limits, an image id), sends it to the device, waits for the restart and reads
INFO: the runtime version and the image id must match. The default route is **MidiRpc** (`--route rpc`): the device needs no menu step, takes the image in
chunks of 2048 bytes, answers every chunk, and checks the whole image. The program asks once, before it sends anything: `Flash Digitakt II OS 1.17 (stock) now? [y/N]` (for a patch: `... OS 1.17 with digikit runtime 0.5.2 now?`); only `y` (or `--yes`) starts. When the device has checked the whole image, the program commits at once and the device erases and writes its flash. `n` sends nothing.

```sh
uv run python -m tools.patchkit.patchkit flash firmware/Digitakt_II_OS1.17.syx --dry-run     # patch and check, send nothing (--save F keeps the image)
uv run python -m tools.patchkit.patchkit flash firmware/Digitakt_II_OS1.17.syx --port Digitakt
uv run python -m tools.patchkit.patchkit flash firmware/Digitakt_II_OS1.17.syx --port Digitakt --restore   # the stock file back
uv run python -m tools.patchkit.patchkit flash firmware/Digitakt_II_OS1.17.syx --port Digitakt --route menu   # the OS UPGRADE menu route (below)
```

`--chunk N` sets the stream bytes per chunk (the largest size a real device takes is not known). It prints the checks and the steps for the device (power
adapter, the cable stays in), asks once, and sends. With `--route menu` it prints the old steps (SETTINGS > SYSTEM > OS UPGRADE until WAITING FOR SYSEX),
waits for the word `flash`, and sends the plain stream one message every 2 ms (no reply exists on that route). The OS UPGRADE screen asks no YES on either
route (finding 75). The section 7 ISA scan (`packages/patchkit/src/dspscan.rs`) is one of the checks (the same scan `patchkit preflight` runs;
`patchkit isa-scan X.syx` lists what it finds).

**The Firmware page** (the site's `/firmware/` and the Firmware item of the workbench) is the same flash in six steps: the stock file, the checks, the
device (the MIDI port, its model and OS), the device's needs (power adapter, cable in; no menu), Flash with a progress bar and Cancel, and the
verdict after the restart (runtime and image id, or for "Restore stock" the stock OS and no runtime). One **Flash** button does all of it: the page sends the image over MidiRpc ("sending 0.68 of 1.88 MB · 12 s left"), the device checks it ("the device is
checking the image…"), and when the device accepts it the page commits at once ("the device checked the image ✓ · flashing, do not unplug"). The click is the
yes; there is no second question. **Cancel** (or leaving the page) works until the device has accepted the image and leaves it as it was ("Nothing was written");
after that the button is gone. Flash is on only when every check
passed and the device is the image's model. A small link, "Use the OS UPGRADE menu instead", brings back the old flow (the box "The device shows WAITING FOR
SYSEX", paced sending, six steps). The page code is `apps/web/src/firmware/` (rules in `firmware-core.ts`, the runs in `firmware-flow.ts`); the checks, the
2 ms sender and the RPC state machine (`patchkit::flash_rpc::RpcFlash`) are the Rust ones (`patchkit.wasm` in the browser and in the workbench).
Proof: `apps/web/tests/firmware-core.test.ts`, `firmware-flow.test.ts`, `firmware-rpc-flow.test.ts`, `firmware-kit.test.mjs` (the wasm image equals `patchkit flash --save`), the
state machine's tests in `packages/patchkit/src/flash_rpc.rs`, and `firmware-emulator.test.ts`, which runs the page's flash against an emulated device
(`tools/emulator/os_upgrade_rig.py ... --rpc --exec`, finding 75). Screenshots of both apps: `node tools/devtools/firmware_shots.mjs --app web|desktop`.

**Flashing with Transfer** (Em's description of the Transfer route; the screens were not checked here):

1. Back up the card and the projects with Transfer. Keep the stock `.syx` at hand. Check the output volume is low.
2. On the device: SETTINGS > SYSTEM > OS UPGRADE. The device waits for a firmware file.
3. In Transfer, open the DROP page and drop `out/patch/dt2-1.17-rungX.syx`. Wait for the device to finish and restart. Do not touch it
   or remove power while it writes.
4. After the restart the device shows the main screen. Rung A: SETTINGS lists DIGIKIT; DIGIKIT > ABOUT shows the runtime; HEALTH shows
   the CPU load and heap; the store line shows "STORE FREE" / "STORE USED" (a read of the store area, no write). Rung B: normal audio from
   a pattern, and HEALTH shows the DSP block time. Rung C: the upload ends with a matching sha256, the module appears in MODULES, after
   APPLY & RESTART the machine plays; HEALTH and ABOUT list it.
5. If the device does not reach the main screen, or a rung misbehaves, go back to stock (below). Tell the maintainer which rung and what
   was on the OLED before taking the next rung.

### Rung C: a machine loaded at run time (Digitakt II 1.17)

Rung C plays an added machine type, `digikit/pluck` (run-time type 18), for the first time on the device. It is the step where another
user's Digitakt II halted its SHARC (finding 45), so the image now records where the DSP is (finding 48): a halt is read from the log, not
guessed. Rehearsed in the emulator on 2026-10-18 with exactly these images and commands (scenario `rung-c`, screenshots in
`out/runs/lanes/b-rungC/`, not committed): every step below ended as described. **The device itself has not run this yet.**

Since 2026-10-21 (P3e-1) the module of these sections is `digikit/pluck` and its file is `pluck-dt2.dkm` (before: `test/dyn-pluck`, `dyn-pluck-dt2.dkm`). The module sizes and sha256 values quoted below were measured before the rename and change with it (the module id and the machine key are in the file): compare with the values `pack` and `shasum` print.

**1. Build and check the image** (the rung B module list; the file is new because the DSP changed):

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rungC-dt2-1.17.syx
mise run patch:preflight -- out/patch/rungC-dt2-1.17.syx     # must end with PREFLIGHT OK; sections 3 and 7 differ from stock, nothing else
mise run module:pack -- digikit/pluck --device dt2           # out/patch/pluck-dt2.dkm
shasum -a 256 out/patch/rungC-dt2-1.17.syx out/patch/pluck-dt2.dkm
```

Image: 1,885,088 bytes, sha256 `fb6caf044c633db18c803b0797731fcfec78a73e9b2e30a717e45c1ecd779cbf`; module: 4,484 bytes, sha256 `e66c4660c028fda721e680f3b1aa0c3106133647f9016aa0531773c1d6e64b41`. (`pack` prints a second sha256, that of the image inside the module; the one to compare is the file's, as `shasum` and `dkupload ls` print it.) A different hash means
the sources changed since this text was written: use the printed value and the preflight result.

**2. Flash it** over rung B (or over stock) the way rungs A and B were flashed: Transfer, SETTINGS > SYSTEM > OS UPGRADE, drop the `.syx`
on DROP, wait for the restart. Keep the volume low. The module store on the card is still blank at this point.

**3. Check the log before any module** (nothing is written to the card by these commands):

```sh
uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt follow --once --out out/runs/device-logs/rungC-1.log
```

Good: `BOOT_START`, `CARD_CHECK state=blank`, `RAM_KEEP`, `UI_FIRST`, no `SAFE`; then one `DSP_DIAG` set (five events, 17 words: `count=17`,
`magic_count=0011d1a6`, `link_beat` and `voice_beat` in the thousands, `M5=0 M6=1 M7=ffffffff M13=0 M14=1 M15=ffffffff`, the same `I6 I7 B7 L7
MODE1` as rung B, `crumb=HOOK_ENTERED voice=31 type=0`, `dispatches=0 returns=0`). After that the log adds a set only every ten seconds or when
a word other than a beat or a counter changes, and a `HEALTH` line every ten seconds.

**4. Upload the module** (the same subcommands as in the rehearsal, `--port Digitakt` where it used `--emu --run-dir`):

```sh
uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt info    # reads only: protocol 1, runtime 3, free_bytes about 12.5 MB
uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt put out/patch/pluck-dt2.dkm
uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt ls      # modules/pluck-dt2.dkm  4484  <the sha256 of the file>
```

`put` ends with `4484 bytes in ... (sha256 checked on the device)`. In the rehearsal the upload wrote 12 sectors, all inside the store area
(the device's `[card.store]`: DT2 `0x2DF1000..0x2E1A000`, DN2 `0x800000` to the card end; the rehearsal ran on the older area `0x38000..0x3e000`); the firmware's own project sectors were not touched. If `put` fails, nothing else has happened: `dkupload ... clear`
empties the store (it changes only that range).

**5. Tick it and restart** (on the device): SETTINGS > DIGIKIT > MODULES. The list shows `[ ] PLUCK-DT2.DKM`. Press **YES** (it shows
`[X]`), press **DOWN** (the cursor goes to `APPLY & RESTART`), press **YES**: the row reads `CONFIRM RESTART` and a popup says `UNSAVED CHANGES LOST`.
Press **NO** to dismiss the popup, then **YES** again on the row: the selection is saved and the device restarts (a few seconds, the OLED goes
dark and the main screen returns).

**6. After the restart:** SETTINGS > DIGIKIT > ABOUT shows `BOOT: 1 OF 1 OK`, `PLUCK-DT2.DKM`, `- OK` and `RAM KEPT: YES` (RAM survived this
restart; `NO` would mean it did not, which is still open on the device). HEALTH (one row down) shows CPU about 20-30 %, DSP about 30-40 %, RAM free
about 26 MB. Log:

```sh
uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt follow --once --out out/runs/device-logs/rungC-2.log
```

Good: `RAM_KEEP kept=1`, `MOD_LOAD index=0 result=1 name=pluck-dt2.dk` (the CF part is in; result 1 means the DSP part follows), a moment later
`MOD_DSP index=0 result=0` (the DSP part is in), `BOOT_RECORD ... selected=1`, no `SAFE`.

**7. Choose the machine on track 1** (user manual A.1): select track 1 (TRK + TRIG 1), press **FUNC + SRC** to open the MACHINE menu (`MACHINE SEL >
TRACK 1`), use **LEFT/RIGHT** to stay in the **SRC** category, press **DOWN** until the cursor is on **PLUCK** (the last entry, below MIDI: seven
presses from the top of the list in the rehearsal), press **YES**. The track page now shows the parameters TUNE, DECAY, PICK, BRIGHT, COLOR and the level knobs. The SRC view of an added machine has **one page** (no page dots; SRC again, UP and DOWN do not change it): a second page that repeats the first is a
bug of an image built before 2026-10-19 (finding 50). Rebuild the image to get one page.
(The emulator rig pressed YES twice, 300 ms apart; if the track page does not show those parameters after one press, press YES once more.)

**8. Play:** press **TRIG 1** (hold it for a few seconds). Expected: a plucked-string tone, decaying (the rehearsal's note is sample for sample the
patched-in `test/patched-pluck`). Keep the key down for 10 to 20 seconds once, then read the log again (command of step 6, new `--out`):

Good log (dispatches and returns rise together; this is the rehearsal's):

```
 3.238s  DSP_DIAG   count=17 chunk=3 L7=000003e2 MODE1=39011cf8 crumb=HOOK_ENTERED voice=31 type=0 dispatches=0
13.272s  DSP_DIAG   ...                                          crumb=HOOK_ENTERED voice=31 type=0 dispatches=0
28.338s  DSP_DIAG   count=17 chunk=3 ...                         crumb=EPILOGUE_DONE voice=1 type=18 dispatches=813
28.338s  DSP_DIAG   count=17 chunk=4 returns=813
38.338s  DSP_DIAG   count=17 chunk=3 ...                         crumb=EPILOGUE_DONE voice=1 type=18 dispatches=15813
38.338s  DSP_DIAG   count=17 chunk=4 returns=15813
```

`dispatches` counts the calls of the run-time routine, `returns` the calls that came back. `dispatches - returns` is 0 (or 1 for a call in flight at
that instant). The `crumb` is the last step the voice hook recorded; it names a registered voice (`EPILOGUE_DONE voice=1 type=18`: both voices of
track 1 are done) or the last voice of the block (`HOOK_ENTERED voice=31`), depending on which of the DSP's two reply records the ColdFire read. No
`DSP_STOPPED`, `heartbeat` words that rise (`link_beat` +1500 a second).

**What a halt looks like.** Audio stops (also for the stock tracks), the UI keeps working, a sample preview freezes the +Drive (finding 45). Within
about four seconds the log gets (rehearsal of a routine that never returns, `test/dyn-dsp-hang`):

```
15.000s !DSP_STOPPED  heartbeat=15560 voice_beat=11157 crumb=ROUTINE_CALLED voice=0 type=19 dispatches=4 resumed=False
15.000s !DSP_DIAG     count=17 ... (five events: all 17 words, from the reply that showed the stop)
15.000s !MOD_LIST     index=0 slots=0 name=...                    (one per loaded module)
15.018s !DSP_DIAG     ... (the other reply record, until both have been seen)
```

and ABOUT shows `DSP STOPPED` and `LAST: CALL V0 T19` (step, voice, type; T18 for the pluck). Read it as: `crumb` names the last step reached (`ROUTINE_CALLED`: the
core is inside the machine's routine; `ROUTINE_RETURNED` or `EPILOGUE_DONE`: it got out and stopped later; `TYPE_FOUND`: between the table lookup and the call;
`HOOK_ENTERED`, an old `EPILOGUE_DONE`: not in a registered voice), `voice` is the voice (track = voice / 2), `type` the machine type, `dispatches - returns = 1`
says a routine never came back, `link_beat` equal in the two replies says the whole block handler froze. If `DSP_STOPPED` shows `crumb=NONE`, the ColdFire got
no diagnostic block at all (the DMA did not keep sending the last record): then the frozen `heartbeat` is all there is. Save the log (`follow --once --out FILE`)
and send it with the ABOUT screen. This is the case the whole of rung C exists to see.

**Back to stock.** Remove the module without a flash: SETTINGS > DIGIKIT > MODULES, **YES** on the ticked row (`[ ]`), **DOWN** to `APPLY & RESTART`, YES, NO, YES
as in step 5; or `dkupload --port Digitakt clear` (empties the store) and restart. To go back to the stock firmware flash `firmware/Digitakt_II_OS1.17.syx`
(shasum `26c22f66...`) the same way as the rungs; the bootstrap recovery in "Recovery" below is the way back from a device that does not start.

**Safe mode.** If a module crashes the boot twice, the third boot is a safe boot (no module loads); a selection is kept. Holding RECORD at
power-on is meant to force it, but that key route is read from code and was never observed **[O]** (finding 37).

**Recovery** (the bootstrap is never changed by patchkit, so it works whatever MAIN OS or the DSP does; see also "Recovery" above):

1. Power off. Hold **FUNC** and power on. The STARTUP menu comes from the bootstrap.
2. Press **TRIG 4** (OS UPGRADE). This receives over **MIDI DIN only**; USB is not available here. Connect a DIN MIDI interface to the
   device's MIDI IN.
3. In Transfer, open the SysEx page, pick the DIN interface and send `firmware/Digitakt_II_OS1.17.syx` (the stock file, not a patched
   one). Wait for the device to finish and restart. The stock firmware runs.
4. A checksum or HMAC failure shows UPGRADE ABORTED and writes nothing; send again. Keep the stock file's sha256
   (`26c22f66...`) next to you: `shasum -a 256 firmware/Digitakt_II_OS1.17.syx`.

### Rung C2 (bisect): the same flow with the wrapper's call path removed (Digitakt II 1.17)

Rung C halts the DSP when a track is on the added type 18 (finding 47). Finding 49 narrows it to two suspects: the **wrapper's own path** for types of 7 or
more (its first `CALL`/`RTS`, and for a registered type the `MODE1` writes, the stores below the frame and the odd-voice loop), or the **stock pipeline** with a
type it never saw. Rung C2 separates them. It is the rung C image with one build option of `core/dsp-voice`, `DV_BISECT_C2`: for a type of 7 or more
the wrapper writes its breadcrumb inline (no `CALL`, no `RTS`) and takes the stock path, and it never dispatches (so no `MODE1` or other system-register
write, no stores below the frame, no loop, no indirect jump). The stock-type path (types below 7) is byte-identical to rung C (proof in finding 49,
section 8). The module `pluck-dt2.dkm` is the same file; it loads and registers its row, and the wrapper ignores the row.

**Build** (the option is a `#ifdef` in the one source file; `pk_variant` builds the objects with it into `out/variants/dv-c2/`, the committed objects are not
touched, and `DIGI_PK_ROOT` makes `patch` and `preflight` use that tree):

```sh
R=$(uv run python -m tools.patchkit.pk_variant make core/dsp-voice DV_BISECT_C2 dv-c2)
DIGI_PK_ROOT=$R mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rungC2-dt2-1.17.syx
mise run patch:preflight -- out/patch/rungC2-dt2-1.17.syx       # PREFLIGHT OK; sections 3 and 7 differ from stock, nothing else
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rungC-dt2-1.17.syx   # the matching rung C
shasum -a 256 out/patch/rungC2-dt2-1.17.syx out/patch/rungC-dt2-1.17.syx
```

Image `out/patch/rungC2-dt2-1.17.syx`: 1,885,472 bytes, sha256 `3a34dcf60697a73fce28c67d476be3b6e421e1d4ea08ddc8f9496e1aa3350174`; the rung C image built
from the same tree for comparison: 1,885,344 bytes, sha256 `ac72f220f3c2f39b22566ec9b23e5884214f4ebebdabc6ddd676d84e1c556f1a` (the hash in "Rung C" above is older: the
tree changed since). Compared by section, the two differ in the DSP section (the wrapper) and in five bytes of MAIN OS (the addresses of the helpers that
follow the wrapper's code, which moved).

**Run** steps 2 to 8 of "Rung C" with this image (flash, log, upload the same module, tick it, restart, choose PLUCK on track 1, hold TRIG 1). Differences you
will see: no pluck sound (the routine never runs; the emulator run did not record what the stock renderer plays for type 18), and in the log the `dispatches` field
counts the **stage 3 passes** of the wrapper (two per block: the two voices of track 1), `returns` stays 0. In the 10 s sets: `crumb=HOOK_ENTERED voice=31 type=0`
(the last voice of the block overwrites the stage 3 word) and `dispatches` rising by about 30,000 per 10 s (1,500 blocks a second times two). In the emulator the
count starts at the first TRIG press (before it, `dispatches` stays 0 with the machine selected); on the device the type may reach the DSP earlier. Leave TRIG 1 held
for at least a minute; the halt of rung C came within seconds.

**Reading the result.**

- **No halt** (no `DSP_STOPPED`, `link_beat` and `voice_beat` rise, `dispatches` rises, the other tracks keep playing): the stock pipeline accepts a run-time
  type, and the cause is in the wrapper's call path: the first `CALL`/`RTS` (the stage 3 path has no `MODE1` write; Em reported the DSP dead after a reboot with the
  track still on type 18 and no module loaded, finding 49 sections 5 and 8), or for a registered type the further steps. The next build puts the `CALL` back for stage 3 only (that is rung C with no module loaded) and then the registered
  steps one by one.
- **Halt** (`DSP_STOPPED`; `dispatches` frozen at its last value, ABOUT `LAST: ...`): the stock pipeline stops on type 18 with the wrapper's path out of the
  way. A frozen `dispatches` that stopped rising at a value N says that N stage 3 passes completed; then the next suspects are in the stock steps of
  finding 49 section 6 (candidates 1 and 3: the frame handling, the change detector at 0x1c33d7, the parameter converter, the selector tables). A halt at once
  (`dispatches=0`, crumb `HOOK_ENTERED voice=31`) says the core stopped before the first stage 3 pass of voice 2: in the frame handler or in the stock code of
  voices 0 and 1.
- Either way keep the log (`follow --once --out out/runs/device-logs/rungC2-N.log`) and the ABOUT screen, and flash `firmware/Digitakt_II_OS1.17.syx` or
  the rung C image back as described under "Back to stock".

### Rung C3: three variants of the registered path (Digitakt II 1.17)

Rung C2 ran for 40 s with a track on type 18 (finding 47): the stock pipeline accepts the type, and the halt of rung C is inside `core/dsp-voice`'s path for a **registered** type,
before its stage-2 breadcrumb. Finding 49 section 9 lists the 69 instructions that never ran on silicon before (the registered prologue and the first ten of `_dv_crumb`) and
builds three images, each without one suspect, all with three **fine breadcrumbs** (stage 8 after the scan matched, 9 after the frame is saved, 10 after the pins) so that a halt that remains names its zone. Not part of
any shipped image; the module and all steps are those of "Rung C".

| variant | removes | image | sha256 |
| --- | --- | --- | --- |
| **a** | the non-delayed `CALL`/`RTS` (delayed forms with two `NOP` slots, as the stock image uses) | `out/patch/rungC3a-dt2-1.17.syx` | `56d1f9945e09c2dab2980a0f80c1084efd86a941fc9e10d8b600fa137c353d55` |
| **b** | nested interrupts across the registered path (MODE1.IRPTEN cleared, entry MODE1 restored) | `out/patch/rungC3b-dt2-1.17.syx` | `5dd9e7a7c459f72e1550069bc66b55023c4e1a3ce39c12ab4f6ea59127834c4e` |
| **c** | the MODE1 writes (`BIT CLR`/`BIT SET`/`NOP` and the restore) | `out/patch/rungC3c-dt2-1.17.syx` | `aaa9f365b229a5a74c0eb5a605f30dd9246786211fa84d96a7ab6c6ffe00c906` |

All three: 1,885,472 bytes, `PREFLIGHT OK` (sections 3 and 7 differ from stock, nothing else), base `26c22f66...`, module `out/patch/pluck-dt2.dkm` unchanged (sha256 `e66c4660...`). All three also restore `B3` before `I3`
(finding 49 section 9.2). Each passes `rung-c3a`/`b`/`c` in the emulator (PCM of the run-time pluck equal to the patched-in pluck, 19,574 dispatches and returns) and the quick `pk_proof_voice` (stock type 0 bit-identical).

**Build** (each variant is `pk_variant make core/dsp-voice OPTIONS LABEL`; `DIGI_PK_ROOT` makes `patch` and `preflight` use it):

```sh
for v in a:DV_C3_FINE,DV_C3_DB b:DV_C3_FINE,DV_C3_NOIRQ c:DV_C3_FINE,DV_C3_NOSYS; do
  n=${v%%:*}; R=$(uv run python -m tools.patchkit.pk_variant make core/dsp-voice ${v#*:} dv-c3$n)
  DIGI_PK_ROOT=$R mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rungC3$n-dt2-1.17.syx
  DIGI_PK_ROOT=$R mise run patch:preflight -- out/patch/rungC3$n-dt2-1.17.syx    # PREFLIGHT OK
done
shasum -a 256 out/patch/rungC3?-dt2-1.17.syx
```

**Run**, for the variants in the order **a, b, c** (stop at the first that does not halt): before flashing, make sure track 1 holds a stock machine (the project with type 18 on a track halts the DSP
at the next boot with the module loaded). Flash the image (as for rung C: "Rung C" steps 2 to 4; the module may still be on the card and selected from rung C2: `dkupload ls`, MODULES, APPLY & RESTART only if it is not),
choose PLUCK on track 1, hold TRIG 1 for **60 s**, keep the log (`follow --once --out out/runs/device-logs/rungC3a-1.log`) and the ABOUT screen, then flash `firmware/Digitakt_II_OS1.17.syx` back (or the next variant).

**Reading the result** (full table: finding 49 section 9.6). No halt and the pluck sounds: the removed suspect was the cause (a: the delayed forms are the fix). Halt: the log's `DSP_STOPPED` names the
last breadcrumb: `C3_TYPE_MATCHED` (8) = the `MODIFY` or the 35 stores; `C3_FRAME_SAVED` (9) = the pins; `C3_CONTEXT_PINNED` (10) = the `CALL` into `_dv_crumb` or its first instructions; `HOOK_ENTERED` (1) = before the first
fine breadcrumb. The ABOUT page shows stages 8 to 10 as `?`.

In the emulator the interrupt probe adds a check of nested interrupts: `[rungc] irq_probe = "both"` in scenario `rung-c-irq` (`SHARC_IRQ_PROBE`, docs/INSTRUMENTATION.md).

### Rung C3d: the 17 stores the ISA does not have (Digitakt II 1.17)

Rung C3a (finding 47) stopped the SHARC after fine stage 8 and before stage 9: inside the registered path's `MODIFY(I7, -40)` and its 35 frame stores. Finding 49 section 10 finds 17 of those stores
are not instructions of the ISA: a DM store cannot take a DAG1 register (I0-I7, M0-M7, L0-L7, B0-B7) as its source (PRM 15-7, 15-8, 15-10, 15-11, 13-13; the encoding class is UREGXDAG1), the stock image has none, and the first one
(`DM(-16, I6) = I0`) is the 12th instruction after the stage-8 breadcrumb. Three images, all with the rung C module list, base `Digitakt_II_OS1.17.syx`, options `DV_C3_FINE` plus (these images were built when two build options selected the legal stores and the epilogue order; both are the only build of the module now, and the `DV_C3_FINE` DSP object is byte-identical to d2's):

| variant | option | what it does | image | sha256 |
| --- | --- | --- | --- | --- |
| **d2** | (none: the default build now) | the 17 stores go through R2 (`R2 = I0; DM(-16, I6) = R2;`) as the stock code does | `out/patch/rungC3d2-dt2-1.17.syx` | `18d7a6d56c537d3df664a5b5edf2931d74ac676b537ad1a318b78d30973cb94b` |
| **d1** | (not buildable any more) | the stores kept as they are; twelve inline breadcrumbs (stages 11 to 22) between them | `out/patch/rungC3d1-dt2-1.17.syx` | `3972e3929bbcde01a3719496b860ecc6c760334be94ee4023df412f6fa6515cf` |
| **d3** | `DV_C3_DEEP` | both | `out/patch/rungC3d3-dt2-1.17.syx` | `0840b3bba8c32fbda7fd7fb98b923b76fb9b411d90efa8c3a9e7a789c860f0fa` |

All three `PREFLIGHT OK` (sections 3 and 7 differ from stock, nothing else); 1,885,600 bytes (d2), 1,885,728 bytes (d1, d3); the module is the unchanged `pluck-dt2.dkm` (`e66c4660...`). Not part of any shipped image.
Emulator (scenarios `rung-c3d1`, `rung-c3d2`, `rung-c3d3`, full tier): see "Emulator proof" below. The emulator accepts the excluded stores, so d1 does not halt there; a pass in the emulator says the
variants keep the path working, not that the cause is found.

**Build** (a module build with a store the ISA does not allow fails, and `patchkit preflight` and every `patchkit flash` check scan section 7 for them: `packages/patchkit/src/dspscan.rs`; d1 can no longer be built, the old image shows 17):

```sh
for v in d2:DV_C3_FINE d3:DV_C3_FINE,DV_C3_DEEP; do
  n=${v%%:*}; R=$(uv run python -m tools.patchkit.pk_variant make core/dsp-voice ${v#*:} dv-c3$n)
  DIGI_PK_ROOT=$R mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rungC3$n-dt2-1.17.syx
  DIGI_PK_ROOT=$R mise run patch:preflight -- out/patch/rungC3$n-dt2-1.17.syx    # PREFLIGHT OK
done
shasum -a 256 out/patch/rungC3d?-dt2-1.17.syx
```

**Run** d2 first (then d3 if d2 halts, d1 if the mechanism is to be shown), exactly as "Rung C3": track 1 on a stock machine before flashing, flash the image, choose PLUCK on track 1, hold TRIG 1 for **60 s**, keep the log
(`follow --once --out out/runs/device-logs/rungC3d2-1.log`) and the ABOUT screen, flash `firmware/Digitakt_II_OS1.17.syx` back (or the next image). `dklog` names the stages 11 to 22.

**Reading the result.**

- **d2, no halt, the pluck sounds, `dispatches` = `returns`:** the cause was the 17 stores. The default build is changed (the legal stores always; done, the Digitone II wrapper too, finding 49 section 10.6): run once more with the default build.
- **d2 halts:** the last breadcrumb names the zone as in the table below (d2 carries stages 8, 9, 10 only: last 8 = `MODIFY` or the stores, 9 = the pins, 10 = the `CALL`); then run d3, which has the stages 11 to 22.
- **d3 or d1, the last breadcrumb, by the instruction group that follows it** (a halt inside the next breadcrumb's own first instructions reads the same way):

| last breadcrumb | the group that did not finish |
| --- | --- |
| 8 `C3_TYPE_MATCHED` | `MODIFY(I7, -40)`, or the breadcrumb 11 code |
| 11 `D_AFTER_MODIFY_I7` | the ten stores `R3` ... `R15` (`DM(-6 .. -15, I6)`; all legal) |
| 12 `D_AFTER_R_STORES` | **`DM(-16, I6) = I0`**, the first DAG1 store (d1: the expected result) |
| 13 `D_AFTER_I0_STORE` | `I1`, `I2`, `I3`, `I5` stores |
| 14 `D_AFTER_I1_I5_STORES` | `M1`, `M2`, `M3` stores |
| 15 `D_AFTER_M1_M3_STORES` | `B3`, `I13` stores |
| 16 `D_AFTER_B3_I13_STORES` | `M5`, `M6`, `M7` stores |
| 17 `D_AFTER_M5_M7_STORES` | `M13`, `M14`, `M15` stores (DAG2, legal) |
| 18 `D_AFTER_M13_M15_STORES` | `L0` ... `L3` stores |
| 19 `D_AFTER_L0_L3_STORES` | `R3 = DM(_dv_sv_l4)` |
| 20 `D_AFTER_SV_L4_LOAD` | `DM(-36, I6) = R3`, `DM(-37, I6) = L5` |
| 21 `D_AFTER_L4_L5_STORES` | `R3 = MODE1` |
| 22 `D_AFTER_MODE1_READ` | `DM(-38, I6) = R3` and the head of stage 9 up to its first record store |
| 9 `C3_FRAME_SAVED` | the pins (`M5 = 0` ... `BIT SET MODE1`) |
| 10 `C3_CONTEXT_PINNED` | the `CALL` into `_dv_crumb` or its first instructions |
| 1 `HOOK_ENTERED` | before the stage-8 breadcrumb |

  A halt in a legal group (11, 17, 19, 21, 22) means the DAG1 stores are not the whole story; a halt that moves with the register class in d1 (12, then 13 ... if the first excluded store is tolerated) says which
  class the silicon refuses. If d3 passes and d2 passes, the deep breadcrumbs cost nothing and either may be kept.

**Emulator proof** (DT2 1.17, the images above, `uv run python -m tools.patchkit.pk_digikit_scen run rung-c3dN --image dt2-1.17 --tier full`):

| scenario | result |
| --- | --- |
| `rung-c3d2` | PASS, 241.6 s |
| `rung-c3d3` | PASS, 242.3 s |
| `rung-c3d1` | PASS, 234.0 s |

Each is the rung C flow with the run-time pluck on track 1: 19,574 dispatches and as many returns, no `DSP_STOPPED`, write-watch and flush checks clean, the note equal to the patched-in pluck.

### Runtime 4: dk run and screenshots (Digitakt II 1.17)

Runtime 4 adds two commands to the upload protocol (SELECT writes the selection record, RESTART does what APPLY & RESTART does; docs/design/digikit-upload.md),
so a module goes from source to running with one command and no menu. One flash of the runtime-4 image, then no further flash for a module change.
Rehearsed in the emulator on 2026-10-19 (scenarios `menu-dkrun-pluck` and `upload-screenshot`); **the device has not run it yet**.

**1. Build and check the image** (the rung C module list; nothing else changed except the runtime, 3 to 4):

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rt4-dt2-1.17.syx
mise run patch:preflight -- out/patch/rt4-dt2-1.17.syx     # PREFLIGHT OK and "ISA check: ok"; sections 3 and 7 differ from stock, nothing else
shasum -a 256 out/patch/rt4-dt2-1.17.syx
```

Image: 1,886,112 bytes, sha256 `17ba5a0714cb0669ecef4ef86a53678def6502eeba3069d3c216efc19ca32324` (a different hash means the sources changed since this text was
written: use the printed value and the preflight result).

**2. Flash it** as before (Transfer, SETTINGS > SYSTEM > OS UPGRADE, drop the `.syx` on DROP). Keep the volume low. A card that already holds the store and the
selection of rung C4 keeps both.

**3. Check the runtime version** (read-only): `uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt info` must print `runtime: 4`.

**4. Run a module** (build, pack, upload if the sha256 differs, SELECT, RESTART, wait for the boot, then the log for 30 s; the Digitakt restarts and its USB port
comes back, the tool connects again within 90 s):

```sh
uv run --with python-rtmidi python -m tools.device.dkrun digikit/pluck --port Digitakt --seconds 30
# or: mise run dk:run -- digikit/pluck --port Digitakt --seconds 30
```

It prints one `dk run:` line per step, then the boot's `MOD_LOAD ... result=1` (the DSP part is loading) and `MOD_DSP ... result=0` (loaded), and exits 1 with the reason
when a step fails (a runtime older than 4 says "flash a runtime-4 image"; a card that is not ours and not blank says so). The selection becomes this module
only. Play a note of the machine on track 1.

**5. Screenshot** (the firmware's own MidiRpc Screenshot, read-only, no runtime needed):

```sh
uv run --with python-rtmidi python -m tools.device.dkscreen --port Digitakt out/runs/shots/screen.png
uv run --with python-rtmidi python -m tools.device.dkscreen --port Digitakt --every 2 --count 5 out/runs/shots/screen.png    # screen-000.png ... screen-004.png
```

The PNG is 512x256 (4 times the OLED); `--scale N` changes it. It sends only a request the stock firmware answers.

### Runtime 5: the DSP guard (Digitakt II 1.17)

Runtime 5 (finding 51) clamps pluck's pitch (the LFO-on-TUNE failure: Inf/NaN in the string state silenced every track) and adds a guard to the voice wrapper
`core/dsp-voice`: the machine's parameter words are clamped to the ranges its `[[param]]` declare, a block that is not finite or above 8.0 is zeroed, the voice ends,
the fault is counted, and a machine type that faults 8 times in a second is switched off until the next start. The log gets `MACHINE_FAULT` and `MACHINE_DISABLED`; INFO says
`runtime: 5`. **The device has not run it**; rehearsed in the emulator and on the SHARC rig (`debug-guard`, `test_dsp_guard.py`).

One flash, and the `.dkm` files must be packed again (a DSP section of version 2 needs runtime 5; `dk run` packs and uploads the new file, because its sha256 differs).

**1. Build and check the image** (the runtime-4 module list):

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp -o out/patch/rt5-dt2-1.17.syx
mise run patch:preflight -- out/patch/rt5-dt2-1.17.syx     # PREFLIGHT OK and "ISA check: ok"; sections 3 and 7 differ from stock, nothing else
shasum -a 256 out/patch/rt5-dt2-1.17.syx
mise run module:pack -- digikit/pluck --device dt2          # the machine as a .dkm (DSP section version 2)
```

Image: 1,887,392 bytes, sha256 `ced22f18eb8c1a955be440b3d6bffe8b776fd00d0ce1d20053d1c90e46f8d16f` (a different hash means the sources changed since this text was written: use the printed value and the preflight result).
`digikit/pluck` for dt2: `d0463cbbfb90db2d196563e23f165cfd3cdf57e1b015151c15d6d956f175edc7` (4,580 bytes, `out/patch/pluck-dt2.dkm`).

**2. Flash it** as for runtime 4 (Transfer, SETTINGS > SYSTEM > OS UPGRADE, drop the `.syx` on DROP). Keep the volume low.

**3. Check** `uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt info` prints `runtime: 5`. A `.dkm` packed for runtime 4 (DSP section version 1) still loads
(every range undeclared, so the guard clamps to 0 .. 0x7f00), but the pluck inside it has the old arithmetic: run `dk run digikit/pluck` again so that the new file is uploaded.

**4. Run the pluck with an LFO on TUNE** (`dk run` as for runtime 4, then the LFO on the TUNE knob of the SRC page, depth over the whole range, a note held on track 1):
the sound follows the pitch to the limits of the string and stays; no track goes silent. Read the log (`uv run --with python-rtmidi python -m tools.device.dklog --port Digitakt`):
there should be **no** `MACHINE_FAULT` (the clamp in the routine prevents the cause). A `MACHINE_FAULT` names the type, the track, the kind (`nonfinite`, `overlevel`) and the knob words of the call; a
`MACHINE_DISABLED` says the guard switched that machine off (it plays silence until the next restart). ABOUT and the `DSP_DIAG` events carry the guard's words 17 to 23 (`modules/core/adapters/dsp-link/README.md`).

### Status

Patched images were flashed to one Digitakt II (finding 47). Whether and when to flash again is the maintainer's decision, every time.
The device on the desk is a Digitakt II on 1.17: the DT2 1.17 port is the one that has been run on hardware.
