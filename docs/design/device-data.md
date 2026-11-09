# Device data: where each device, board and firmware fact lives

Status: implemented in Phase 8 (lane b). Rule: a fact about a device, its board or its firmware that the digikit runtime, the core
modules, patchkit or `port_resolve` use comes from a data file, and code that needs a constant gets it generated
(`patchkit gen`, kept current by `patchkit gen --check` and the Rust test `packages/patchkit/tests/devdata.rs`). Nothing in the code
assumes exactly two devices. Syntakt is not added here.

## Where each fact lives

| Fact | Source (data) | Used by | How it gets there |
|---|---|---|---|
| Short name, long name, firmware list (sha256, version) | `devices/<dev>.toml` `[device]`, `[[firmware]]` | everything | read directly |
| SysEx device byte of the `.syx` framing | `[device] syx_id` | patchkit (`syx.rs`, `devdata.rs`) | embedded by `packages/patchkit/build.rs` |
| Device id and name digikit uses (runtime, `core/dynreg`, `core/dyndsp`, `.dkm`, the INFO reply) | `[digikit] id`, `name` (at most 3 characters) | patchkit `register.rs`, `dkmdsp.rs`; the device objects | `devdata.rs` (embedded); `DEVICE_ID`, `DEVICE_NAME_WORD` in `build/gen/device-<dev>.inc` |
| Module store: first sector, length, header B, selection record copies | `[card.store]` | runtime (`dkrt_sdk::store`, `sel`), tools | `modules/core/runtime/sdk/src/device.rs` (`STORE_*`); Python: `tools/patchkit/pk_device.py` |
| Working-project sector (firmware area above the store) | `[[card.regions]]` role `working-project` | `pk_digikit_scen.py` | `pk_device.working_project_sector()` |
| Run-time machine type range (`dynamic_first ..= last`) and with it the number of reserved machine rows and voice rows | `modules/core/adapters/machines/types.toml` | runtime `dsp.rs`, `core/dynreg`, `core/dyndsp` | `DYN_TYPE_FIRST`, `DYN_TYPE_LAST`, `DYN_TYPES` in the generated files |
| DSP area size (window 0) and frame tail geometry | `[digikit] dsp_area_bytes`, `dsp_tail_off`, `dsp_tail_words` | `core/dsp-link` DSP object, `core/dyndsp` table | `DL_AREA_BYTES`, `DL_AREA_WORDS`, `DL_TAIL_*` in `build/gen/device-<dev>.inc` (SHARC `#define`s and gas `.equ`s) |
| Console UART, DDR window and uncached alias, eSDHC controller | `[board]` | `tramp.s` (soft reset), `boot.rs` (`key_held`), `lib.rs` (`card_busy`) | `CONSOLE_UART_BASE`, `DDR_*`, `ESDHC_BASE` in `device.rs` / `build/gen/device.inc` |
| Safe-mode key (a panel control code) | `[digikit] safe_key` | runtime boot | `SAFE_KEY_CODE` |
| RAM that the firmware and boot code leave alone and that the emulator keeps across a soft reset (the crash record, the keep probe) | `[digikit] keep_base`, `keep_bytes` | runtime `crash.rs`; emulator (`device-profile` `keep_ram`) | `KEEP_BASE`, `KEEP_BYTES` in `device.rs` / `device.inc`; the emulator reads the file |
| RAM ranges the PEEK command may read (at most four `[lo, hi)`: the firmware's data, the runtime's region with the keep area; no peripheral registers) | `[digikit] peek` | runtime `upload.rs` (`cmd_peek`) | `PEEK_N`, `PEEK_LO_i`, `PEEK_HI_i` in `build/gen/device-<dev>.inc`; `device.s` lays them out as `dkrt_peek_ranges` (the ranges differ per device, so they are a per-device object, not a shared constant) |
| DSP clock and blocks per second (so cycles per block) | `[board] dsp_clock_hz`, `dsp_blocks_per_second`. The clock word in the start-up code decodes to 1 GHz **[D]** ([finding 100](../findings/100-module-limits.md); the control-register write is not traced). | runtime HEALTH figures | `DSP_BLOCK_CYCLES` (rounded) |
| Container and section limits of a port, DSP L2 range and its excluded ranges, MAIN OS load address | `[patch]` | `port_resolve.py` (written into `devices/ports/*.toml` `[limits]`), patchkit `verify.rs` (an image with no port) | `port_resolve.load_profiles()`; `devdata.rs` |
| Which firmware has a port | `[[firmware]] port = true` | `port_resolve resolve-all` | read directly |
| Device list for manifests (`devices = [...]`), `pack --device` | all device files | `prim.rs`, `module.rs`, `main.rs` | `devdata::by_short` |
| Per-device knob and frame layout of machines | `[modules.knobs]`, `[modules.frame]` | typed sections of modules (`typed.rs`) | existing generator (`consts.rs`, `<dev>.inc`) |

CPU-architecture registers of the MCF5441x itself (the UART status register offset, `RCM.RCR`, the eSDHC `PRSSTAT` offset) are
named constants in the code (`MCF5441X_*`): they are the same on every board that uses the chip. Which UART is the console, where
DDR is and which eSDHC holds the card are board choices and live in `[board]`.

## What `[[gen]]` does

A module asks for generated files with `[[gen]]` in its `module.toml` (`kind`, `path`). `patchkit gen` writes them from the device
files and `types.toml`; `gen --check` fails when one is stale. The kinds (`packages/patchkit/src/devgen.rs`):

- `rust`: `pub const`s in one file, for a crate that is one image for all devices (the runtime). Only constants that every device
  file agrees on are written; a difference is an error naming the constant. Used for `modules/core/runtime/sdk/src/device.rs`.
- `gas-shared`: the same constants as `.equ` lines for ColdFire assembly shared by all devices (`tramp.s`).
- `gas`: `.equ` lines, one file per device (`path` contains `{dev}`), for the device objects: `.include "device-dt2.inc"`.
- `sharc`: `#define` lines, one file per device, for DSP sources (`#include "device-dt2.inc"`). Each note is on its own line:
  the assembler's preprocessor keeps a trailing comment in a macro body, which swallows the rest of a line that uses the macro.
  `.PKBSS name, MACRO;` takes a `#define`d number as its size (`packages/patchkit/src/build/sharc_obj.rs`).

Because the runtime object is built once and used on every device, a constant it uses as an immediate must be the same on all
devices. If a new device needs a different store, a different safe key and so on, that fact has to become a per-device value
that the runtime reads from its device object (like `dkrt_device_id`), which changes the runtime's code; `gen` refuses to write the
shared file until it is done.

## Adding a device (what exists today, what is still hand work)

1. `devices/<name>.toml`: `[device]` (name, short, `syx_id`), `[[firmware]]` entries (add `port = true` when a port is wanted),
   `[board]`, `[digikit]`, `[patch]`, `[card]` with `[card.store]`, `[modules.knobs]`/`[modules.frame]` for machines, `[panel]`.
   Rebuild patchkit (the device files are embedded). The code refuses a device whose `syx_id` or `[digikit] id` repeats.
2. The container seal key of the device in `SEALS` (`packages/patchkit/src/syx.rs`). It is key material of the firmware format, not
   device-profile data, and the device files are published as they are, so it stays in code; a device without an entry is
   refused with a message that says so.
3. `modules/core/adapters/machines/types.toml`: a `[<dev>]` table (`last`, `dynamic_first`, `alloc`, `dynamic`).
4. Modules that have per-device sources add an `[[object]]` with `devices = ["<dev>"]` and a small source for the device (the
   device objects of `core/runtime`, `core/dynreg`, `core/dyndsp`, `core/dsp-link` include the generated `device-<dev>.inc`).
   `core/dynreg` and `core/dyndsp` need `DYN_TYPES` rows each (`tests/patchkit/test_pk_device.py` checks it).
5. `patchkit gen`, `uv run python -m tools.patchkit.pk_build build-all`, `uv run python -m tools.patchkit.port_resolve resolve-all`.
   The anchors of a new firmware are rules in `devices/fw/` ([firmware-decls.md](firmware-decls.md)).
6. Still outside the data: `pk_harness.py` (firmware table, panel buttons), the `Fixed(...)` address tables of the emulator and the
   web app's device list (`apps/web/src/data/device-profiles.ts`).

Facts left in code on purpose: the DT2-only project seeding of `plusdrive` (a capability limit, not data), the container seal keys
(above), and the register offsets of the MCF5441x.
