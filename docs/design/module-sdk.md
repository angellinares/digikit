# Module SDK: one contract per module type, one adapter per device

Status: design, 2026-10-19. Nothing here is built. Nothing was run to write it; every claim about today's code cites file:line, read in the
tree at `f02cf0ac`. Marks as in AGENTS.md: **[D]** read once here, **[O]** open. Proposals are written as "proposal"; they are not facts. Decisions of 2026-10-20: section 11.5.
Related: `docs/plans/digikit-runtime.md` (Phase 11), `docs/design/digikit-dkm.md`, `digikit-runtime.md`, `digikit-registry.md`,
`digikit-store.md`, `digikit-upload.md`, `device-data.md`, finding 27 (typed schema), finding 51 (guard).

Note (2026-10-27): this document is history for the manifest sections. Machines are now declared in C with `DK_MACHINE`, `DK_PARAM` and
`DK_PAGE` (`sdk/include/dk/machine.h`), and effects with `DK_FX`, `DK_FX_PARAM` and `DK_PAGE`. The manifest sections `[[voice]]`, `[[machine]]`,
`[[param]]` and `[[page]]` are refused, and `build/gen/consts.rs`, `dt2.inc`, `dn2.inc` and `module.h` are gone. Current rules:
`docs/MODULE-AUTHORING.md`, section 5.

## 1. Today

### 1.1 Module kinds

| Kind | Example | What it is | Loaded how |
| --- | --- | --- | --- |
| framework | `core/runtime`, `core/dynreg`, `core/dyndsp`, `core/dsp-link`, `core/dsp-voice`, `core/machines`, `core/params` | has firmware hooks, anchor rules, patch ops | patched into the image (`modules/core/*/module.toml`) |
| content, patched in | `test/patched-pluck`, `test/xslice` | machine rows and a DSP routine, placed by patchkit | patched into the image |
| content, run time | `digikit/pluck`, `test/dyn-xslice` | the same machine as a `.dkm` (`dynamic = true`) | `dkrt_load` at boot (`docs/design/digikit-runtime.md:161-183`) |
| ColdFire service | `test/c-demo`, `test/rust-demo` | `dkm_init`, `dkm_fini`, `dkm_slot_n` | `.dkm` (`docs/design/digikit-dkm.md:73-75`) |
| fixture | `test/dyn-guard-nan`, `dyn-dsp-hang`, `dyn-fault`, `dyn-crash`, `twin-*`, `dn2-*` | framework tests, today beside real modules | both ways |

`tests/fixtures/modules/` holds 23 directories; about 15 of them are fixtures, not modules (`ls tests/fixtures/modules`).

### 1.2 Build path

`src` -> `tools/patchkit/pk_build.py` (Selache assembler for SHARC+, m68k-elf gcc or rustc_codegen_gcc for ColdFire; each instruction re-decoded by our
decoders, `pk_build.py:1-17`) -> committed `build/*.dko.json` (our JSON object format; `--check` keeps it current, `pk_build.py:14-16`) ->
`patchkit patch` (links into an image) or `patchkit pack MODULE --device dt2|dn2` (one ColdFire object plus at most one DSP object per device into a
`.dkm`, `packages/patchkit/src/main.rs:370-482`; one compiled module for all devices: D4, section 11.5) -> store on the card (PUT/SELECT, `digikit-upload.md`) -> boot-time `dkrt_load`
(`digikit-runtime.md:161-183`). The browser runs `patchkit.wasm` over the committed objects, so it needs no toolchain (finding 27, `27-module-schema.md:16-19`).

The manifest `module.toml` mixes three things: author data (name, params), device data (templates, knob codes, `clone_of`, `position`, `own_dsp`) and
build data (`place = "dsp.l2"`, `devices`, `[object.externs]`). Example: `tests/fixtures/modules/patched-pluck/module.toml:28-49` has `knobs = [0xca, "pluck_decay", ..., 0xd1]`
(stock codes), `:51-62` has `template = 0xd1` and `[param.dn2] template = 0xd5`, `:46-49` has `position = 4`, `own_dsp = true`.

### 1.3 Two copies, two routines

| Item | Patched in | Run time |
| --- | --- | --- |
| module | `tests/fixtures/modules/patched-pluck` (builds from `../../../../modules/machines/pluck/src/dsp/pluck.s`) | `modules/machines/pluck/module.toml:23-34` builds from `src/dsp/pluck.s` and `pluck_dn2.s` |
| type number | 9 (`types.toml:27`, DN2 :54) | 18 on DT2 (`types.toml:41`), 17 on DN2 (`:67`) |
| DSP routine | `pluck.s` (DT2, 432 lines), `pluck_dn2.s` (DN2, 419 lines) | the same two files |

The two routines share the string kernel. `diff pluck.s pluck_dn2.s` shows 153 changed lines in 15 hunks: header comments (1-24), the prologue (32-99: record, note-on
test, state row), and the tail (406-423: `_pk_idle`, ACTIVE write, return). Lines 100-405 (excitation, coefficients, loop) differ at 220 and 267-272 only **[D]**.
The two files differ in the engine ABI, not in the algorithm.

### 1.4 The two device ABIs of the routine

Both wrappers are in `modules/core/adapters/dsp-voice/src/dsp/`. A routine is entered with `JUMP (M13, I12)` and returns with `JUMP (M13, I13)`
(`dsp-voice.s:18`, `dsp-voice-dn2.s:19`). Both pin M5=0, M6=1, M7=-1, M13=0, M14=1, M15=-1, L0-L5=0, MODE1 = SIMD off, ALUSAT and TRUNCATE clear, RND32 set
(`dsp-voice.s:26-33`, `dsp-voice-dn2.s:24-32`) and save the firmware's values. Both: the routine may clobber R0-R15, I0-I5, I13, M1-M4 (DT2 also B3; DN2 also I12)
and must keep I6-I12/I14/I15, M0, M5-M15, the L registers and MODE1 (`dsp-voice.s:23-25`, `dsp-voice-dn2.s:20-23`).

| Item | Digitakt II (`dsp-voice.s`) | Digitone II (`dsp-voice-dn2.s`) |
| --- | --- | --- |
| hook | first instruction of the voice renderer, `anchor dsp.voice.fn.render_entry` (`module.toml` of dsp-voice :141-149) | first instruction of a helper the engine calls once per voice buffer after the FM synthesis (:181-192) |
| call unit | 32 voices, two per track (2t, 2t+1); routine runs on the even voice only (`dsp-voice.s:14-16`) | 16 voice slots v, one call each (`dsp-voice-dn2.s:11-13`) |
| odd voice | adapter copies the even block (`dsp-voice.s:831-845`) | none |
| registers in | R4 = voice record (0x1d8 bytes), R8 = output block (32 floats), R12 = 32, R0 = type, R1 = voice index (even), R2 = track t (`:21-22`) | R4 = voice buffer (32 floats, 48 kHz), R0 = type, R2 = slot v, R12 = 32 (`:21`) |
| output | 32 floats written in full; engine stages follow | 32 floats written in full; stages follow (gain, amp envelope, gate, filter, overdrive) (`:3-9, 35-36`) |
| note-on | firmware stores 1 at record +0x1b8, clearing byte +0x1b9; `+0x1b8 != 0 and +0x1b9 == 0` is a note-on, the routine writes a mark to +0x1b9 (`:37-39`) | rising edge of bit v of frame halfword 0x11, one block long; the routine detects it itself (`pluck_dn2.s` header, :1-14) |
| voice end | the routine writes ACTIVE = 0 (+0x1b8) | none: the routine goes silent; the engine's envelope and gate are the engine's |
| state | per track, in the routine's own L2 rows (`pluck.s:431-432`: 16 rows, 16 lines of 4096 samples) | per slot, own rows (16 slots; `pluck_dn2.s:419`: the same 256 KiB line memory) |
| frame words (halfwords of the DSP's frame copy) | pitch `0x02`-byte form: hw `1 + t`, velocity hw `0x1a + t`, type = type cache word; knob j of track t at `0x6d + 48 t + j + 2 (j >> 2)` (`pk_conform.py:84-95` Frame, `machine_rig.py:16-17`; `devices/digitakt-ii.toml:298-303`) | pitch hw `1 + v` (note << 8), velocity hw `26 + v`, type hw `74 + v`; knob (slot s) at `0x6d + 0x49 v + (s - 25)` (`dsp-voice-dn2.s:33-35`; `digitone-ii.toml:289-293`) |
| tune | knob 0 is the stock TUNE (code 0xca), 64 << 8 at rest (`pluck.s:22-24`) | no tune knob; x = pitch / (256 * 12) (`pluck_dn2.s` header) |
| level | knob 7 = stock LEV (0xd1), applied by the engine | the engine's level, last knob |
| guard | yes (finding 51; `dsp-voice.s:745-806`, `:898-935`) | none: no record to end, different knob positions (`51-pluck-tune-guard.md:64-66`) |
| helpers | `_dv_exp2`, `_dv_frame_hw`, `_dv_frame_sw` (`dsp-voice.s:1213-1270`) | the same, in the DN2 object (`dsp-voice-dn2.s:260-300`) |

Both devices block: 32 samples per call, 1500 blocks per second (`devices/*.toml [board]`, `pk_conform.py:13-15`).

### 1.5 What the runtime tracks today

| Item | Where | Limit |
| --- | --- | --- |
| module records (name, base, size, slot mask, store path) | `State.modules`, `DKRT_PATHS` (`modules/core/runtime/src/rs/src/lib.rs:75-99`) | `MAX_MODULES = 8` (`lib.rs:39`) |
| ColdFire arena | first fit, 16-byte blocks, `DKRT_ARENA` in bss (`arena.rs:1-22`) | `ARENA_BYTES` 512 KiB, `STAGE_BYTES` 64 KiB (`lib.rs:36-38`); the same on both devices |
| run-time machine types | `core/dynreg` reserves rows 16-23 (`digikit-registry.md:42-50`) | 8 machine rows, 16 parameter rows, 8 page rows (DN2) (`:117-119`) |
| voice rows (DSP) | `voices.rows` of `core/dsp-voice`, 16 words each (`module.toml` of dsp-voice :63-136) | at most 16 registered types (:63-68); run-time types 16-23 (`dsp.rs:23`, `types.toml:20-22`) |
| DSP placement | `DKRT_DSP.mod_off/mod_len`, first fit, 64-byte granules, aligned (`dsp.rs:27, 260-285, 474`) | `dsp_area_bytes`: DT2 524288, DN2 327680 (`digitakt-ii.toml:94`, `digitone-ii.toml:88`) |
| slots | 8 function pointers; slot 0 per UI pass (`digikit-runtime.md:87-94`) | 8 |
| health | `entered[]`, `DKRT_DSP` counters, `dl_diag` words 17-23: fault count, disabled types (`51-pluck-tune-guard.md:59-62`) | |
| reports | INFO: free bytes, runtime version, flags; LIST: store files (`upload.rs:470-496`, `digikit-upload.md:61-62`) | the DSP free/largest figures are in `DkrtStatus`, not in INFO |

The module table is three tables that agree by construction (module records, `DKRT_DSP`, registry rows `DKRT_REG`), not one table with one owner.

### 1.6 What is checked, and when

| When | Check | Where |
| --- | --- | --- |
| build | every instruction decodes with the extent the assembler chose; relocated fields read back | `pk_build.py:9-11` |
| build | typed manifest: unknown fields, knob counts, device models | `typed.rs:26,49,65,178-236` (`deny_unknown_fields`) |
| build / `module:check` | conformance on the concrete core: returns, context registers, finite, bound 8.0, budget `headroom/16`, retrigger, long notes | `pk_conform.py:1-24`, `machine_rig.py:1-20`, `docs/TESTING.md:140-167` |
| pack | one ColdFire object, one DSP object per device; relocation kinds; no PC-relative import | `main.rs:370-482`, `digikit-dkm.md:57-62, 107-111` |
| load | SHA-256, format, `runtime_min`, imports, slots, name, DSP section (device, bounds, imports, rows, types, space), registry records; nothing is written before the last check | `digikit-dkm.md:131-147` |
| run time | knob words clamped to the declared range; output must be finite and below 8.0; 8 faults in 1500 blocks disable the type | `51-pluck-tune-guard.md:36-62`, DT2 only |

Not checked anywhere: that a routine writes only its own memory, a worst-case cycle count (the rig measures a sample of cases), the register rules by static
analysis (the rig checks them on executed paths only), that parameter ranges in the manifest match what the routine assumes (finding 51 found TUNE out of range
reaching an unstable filter; the guard now clamps), the parameter kind (`pk_conform.py:11`: "Parameter kinds do not exist in the manifest yet").

## 2. Goals and non-goals

Goals

1. A content module implements a type contract and nothing else. It contains no firmware address, no engine frame offset, no firmware code.
2. One routine source serves both devices when the contract allows it. For `pluck` it does (section 10; one compiled module for all devices: D4, section 11.5).
3. One adapter per device, driven by the device profile, is the only code that knows the engine layout.
4. The runtime knows every loaded module, its memory, its rows and its limits in one table, and refuses a load that would overlap or exceed.
5. As much of the contract as possible is checked at build time; what cannot be (cycles on a data-dependent path, memory writes through computed pointers) is
   bounded at run time by the guard.
6. The SDK is small and regular: one header per type, one manifest schema, one authoring guide, generated constants.

Non-goals

- No change to stock behaviour. An image with the framework and no content module plays the stock audio (`dsp-voice` README :5-6). Oracle in section 10.
- No Digitone II feature that the Digitakt II does not get; Digitakt II first, Digitone II through the same contract.
- Browser patching keeps working (section 10.4).
- No hot reload (archived, `digikit-runtime.md:116-117`), no memory protection (none exists on the SHARC; the ColdFire MMU is not used, **[O]** whether it could be).
- No FX, sequencer or UI contract is designed now (outlines only, section 4.2).
- No run-time compilation, no DSP intermediate language (`docs/plans/digikit-runtime.md:466-467`).

## 3. Two layers and the tree

| Layer | Contains | Knows | Changes with |
| --- | --- | --- | --- |
| framework | runtime (ColdFire, Rust), registry, DSP link, one adapter per device and type, hooks, anchors, patch rules, the guard | engine frame layout, firmware codes, hook sites, memory map | a firmware port, a device, a contract version |
| content | machines, FX, others: source, manifest, tests, gallery | the type contract and its header only | nothing from a firmware |

Proposal for the tree (`modules/` keeps its role; fixtures move out):

```
sdk/                         the contract, the only thing a content author includes
  dk_machine.h               C header of the Machine contract (section 4.1); the asm include is generated from it
  dk_machine.inc             generated: .equ / #define for assembly (offsets, status codes, register rules)
  dk_machine.rs              generated: Rust SDK types (manifest, parameter kinds, status, header checks)
  dk_fx.h                    the FX contract, version DK_FX_V1 (section 4.2); dk_fx.inc is generated from sdk.toml [fx]
  README.md                  the authoring guide (docs/MODULE-AUTHORING.md moves here or is generated from it)
modules/
  core/<fw>/                 framework modules, as today (runtime, dynreg, dyndsp, dsp-link, machines, params, ...)
  core/adapter-machine/      the Machine adapter: src/dsp/dt2.s, src/dsp/dn2.s (from core/dsp-voice), generated profile constants
  digikit/pluck/            content: module.toml, src/dsp/pluck.s (one source), tests/, gallery/
tests/fixtures/modules/      framework fixtures (dyn-guard-nan, dyn-dsp-hang, dyn-fault, dyn-crash, c-demo, ...), same layout, not selectable by name from `module:*`
devices/<dev>.toml           device profile; gains [adapter.*] and [limits] tables (sections 5, 6, 7)
```

`core/dsp-voice` is renamed or split into `core/adapter-machine` (the hook and the adapter) with the two `.s` files kept per device. The fixtures keep
building through the same pk_build path; only their root changes. `tools/patchkit/pk_conform.NEGATIVE` (`:58-61`) keys on fixture ids and moves with them.

## 4. The contract

### 4.1 Machine (a voice that renders 32 samples)

Rules in words, then the header.

| Party | Guarantees |
| --- | --- |
| adapter, every call | `params[i]` is inside the declared `[min, max]` of parameter i (clamped, so the module never sees an out-of-range word); `pitch` is in `[0, 127 << 8]` after tune; `out` is 32 floats, 4-byte aligned, written by the module in full; the block size is 32 samples at 48 kHz on both devices; context registers are pinned as in section 1.4; one call at a time per voice; calls for one voice come in order |
| adapter, note-on | `state` is clean for `state_clear_bytes` bytes (zero) before `dk_machine_note_on`; `note_on` is followed by `render` in the same block; a voice that returned `DK_DONE` gets no call until the next note-on |
| adapter, after the call | the output is checked (finite, bounded); a fault zeroes the block, ends the voice, resets the state and counts (finding 51); 8 faults in 1500 blocks disable the machine |
| module | `render` returns within `max_cycles_block` (declared); the output is finite and within `+-DK_LEVEL_BOUND` (8.0); it writes only `state`, `out`, its own stack and its own data and bss (shared by all voices); it keeps the register set it must keep; it reads no frame word and no address it did not get; it needs no value of `state` beyond `state_clear_bytes` before it is written; idle voice (before note-on or after `DK_DONE`) is never called, so it has no idle path |

`state` outside `state_clear_bytes` is scratch: it is written before it is read. The rig poisons it with NaN, as it poisons `.PKBSS` today
(`machine_rig.py:11-13`), so a violation shows as a non-finite sample. Pluck's delay line fits this: the line is filled at note-on before the loop reads it.

```c
/* sdk/include/dk/gen/machine.h  --  contract version DK_MACHINE_V1.  Assembly and C implement it; tools generate dk_machine.inc and dk_machine.rs from it. */
#include <stdint.h>

#define DK_MACHINE_V1      1
#define DK_BLOCK           32          /* samples per call, 48 kHz, both devices                  */
#define DK_MAX_PARAMS      8           /* hard cap; the device profile gives the real maximum     */
#define DK_MAX_VOICES      16          /* voices a machine runs at once on any device             */
#define DK_LEVEL_BOUND     8.0f        /* |sample| above this is a fault (+18 dB over full scale) */

typedef enum {                         /* returned by dk_machine_render                           */
    DK_OK    = 0,                      /* block written, voice continues                          */
    DK_DONE  = 1,                      /* block written (may be silence); voice has ended         */
} dk_status;

typedef enum {                         /* parameter kinds: decide the formatter (device profile), the conformance cases, the default range */
    DK_P_PERCENT = 0,                  /* word 0 .. 0x7f00, UI 0 .. 127, plain                    */
    DK_P_BIPOLAR = 1,                  /* 0x4000 at rest, UI -64 .. +63                           */
    DK_P_ENUM    = 2,                  /* n steps, word = step * 256                              */
    DK_P_PITCH   = 3,                  /* semitone offset, 0x4000 at rest; the adapter adds it to `pitch` (the stock tune knob) */
} dk_param_kind;

typedef struct dk_voice_in {            /* per call, built by the adapter                                         */
    int32_t  pitch;                     /* semitones << 8, MIDI note 60 = 60 << 8, tune already added            */
    int32_t  velocity;                  /* 0 .. 127 << 8                                                          */
    uint32_t block;                     /* blocks since note-on, 0 on the note-on block; wraps at 2^32            */
} dk_voice_in;

/* Static description, one per module, in the read-only data section; the packer reads it by the symbol `dk_machine_desc`. */
typedef struct dk_machine_desc {
    uint32_t magic;                     /* 'DKMA'                                                                 */
    uint32_t contract;                  /* DK_MACHINE_V1                                                          */
    uint32_t n_params;                  /* 0 .. DK_MAX_PARAMS; the order is the manifest's `params` order        */
    uint32_t voices;                    /* 1 .. DK_MAX_VOICES the module supports (the adapter uses min(this, device)) */
    uint32_t state_bytes;               /* per voice, multiple of 4                                               */
    uint32_t state_clear_bytes;         /* leading bytes the adapter zeroes at note-on, <= state_bytes            */
    uint32_t max_cycles_block;          /* worst-case instructions of one render call, from the rig, rounded up   */
    uint32_t max_cycles_note_on;        /* the same for the note-on block (note_on + render)                      */
    uint32_t max_call_depth;            /* nested CALLs the module uses (SHARC call stack)                        */
    uint32_t stack_bytes;               /* data stack the module uses (0 for assembly without a frame)            */
} dk_machine_desc;

/* Entry points (global symbols). Register convention on SHARC+: arguments R4, R8, R12, result R0 -- the same as the public compiler's. */

/* Called once per voice, at note-on, after the adapter cleared `state_clear_bytes`. Sets up the string, the excitation, the phase.
   `params` are the values of the first block. Must not produce sound. Returns 0, or a negative number: the adapter treats it as a fault. */
int32_t dk_machine_note_on(void *state, const dk_voice_in *in, const int32_t params[DK_MAX_PARAMS]);

/* Called once per block while the voice runs, including the note-on block, after note_on.
   Writes DK_BLOCK floats to `out`. Returns dk_status. */
int32_t dk_machine_render(void *state, const dk_voice_in *in, const int32_t params[DK_MAX_PARAMS], float out[DK_BLOCK]);
```

Notes on the header:

- No `init`. A module has no state outside `state`. Constant tables are read-only data in the image; the runtime relocates them (`digikit-dkm.md:107-111`).
  A module with a global that it writes fails the write-set check (section 8).
- No voice index. The adapter hands each voice its own `state` block, so a module cannot reach another voice's state, and the Digitone II's slot index and
  the Digitakt II's track index do not appear in module code.
- `params` is an array of 32-bit words in the engine's unit (0 .. 0x7f00 for a percent), which is what `pluck` reads today (`pluck.s:24`). A float view is a
  macro in `dk_machine.inc`, not a second ABI.
- One C-style header for both languages. For assembly, `dk_machine.inc` defines the offsets of `dk_voice_in` and `dk_machine_desc`, the status codes, and two macros
  `DK_ENTER` and `DK_RETURN` for the entry convention (section 5.4). For Rust (ColdFire side and host tools), `dk_machine.rs` is generated by a small tool
  (`tools/patchkit/pk_sdkgen.py`, reads the header's enums and structs; no C parser beyond that subset) and carries the manifest types, the kind enum with its
  default ranges, `dk_machine_desc` for the packer, and the status codes.
- Register rules (SHARC+), unchanged from today: may clobber R0-R15, I0-I5, M1-M4, B3; must keep I6-I15 (I13 is the adapter's return, section 5.4), M0, M5-M15, L0-L7,
  MODE1 (pinned values in section 1.4). A C build adds the frame discipline: I6 and I7 change only by the frame instructions. Checked statically (section 8).
- Cycles: `max_cycles_block` is a declared number. The conformance rig measures the maximum over its cases and fails when it exceeds the declaration or the budget
  (`pk_conform.py:13-23`); the declaration is what the runtime reports and what the adapter's budget table sums (section 6).

### 4.2 Other types (outlines; to be designed when the first module of the type is built)

| Type | Shape of the contract | Open points |
| --- | --- | --- |
| FX (insert or send) | **Built (P3d-1a, `sdk/dk_fx.h`, patched-in only).** `process(state, in, params)`: `dk_fx_in` = `{in, out, block}`, the master block as 64 planar floats (32 left, then 32 right), `in` read only, `out` the adapter's scratch; `dk_fx_desc` = magic, contract, `n_params` (at most 8, 4 in the first rows), `state_bytes`, `max_cycles_block`, `max_call_depth`, `stack_bytes`. The adapter `core/fx` hooks a post-master function (finding 55), checks the output (finite, below 8.0), plays dry on a fault, crossfades one block when an effect switches, and switches an effect off after 8 faults in 1500 blocks. Run-time (`.dkm`) effects: built on the Digitakt II (F4 step 1, `[contract] fx = [1, 0]`, runtime 0.4.0: DSP section version 4, `core/dynfx`, 3 slots; finding 55). Effect pages and knobs: built on the Digitakt II (F4 step 2, runtime 0.5.0: `core/fxpages`, EFFECT record, MIX in the adapter; below). Knob values are saved with the kit (F4 step 3, Digitakt II). Insert or send (FX routing steps 1 to 4, Digitakt II; step 6: also the Digitone II, one send effect, finding 67): the manifest gives the default route and the allowed ones (`[[fx]] route`, `routes`), the user decides the route (a setting, DIGIKIT > FX ROUTES, effective after a restart) and the framework owns the send amounts (one knob per send effect on a second page of the FX key, one amount per track, kept in the settings store and the DSP row, never in a kit file); the effect's `process` is the same and never sees an amount (findings 62, 64 and 65). All of it also on the Digitone II 1.12 (F4 step 5: the same modules, device facts in `devices/digitone-ii.toml`; in the `device` patch set, not in `emulator`: the hook costs the emulator x2.4 DSP CPU there, finding 55). Not built: `tail_blocks` | send versus insert; latency declaration |
| ColdFire service | `dkm_init`, `dkm_fini`, `dkm_slot_0` as today (`digikit-dkm.md:73-75`), plus a declared slot use and stack/heap bytes; Rust or C | which slots exist beyond 0 (`digikit-runtime.md:90-91`: 1-7 are bound and not called) |
| UI page / key handler | needs slots 1-7 from the sequencer or key hooks | not designed; no firmware hook exists |
| Sample / data | no code; a `.dkm` with a data section | store layout (`digikit-store.md`) |

#### 4.2.1 An effect with a page of its own (Digitakt II, F4 step 2)

Note: effects are now declared with `DK_FX` in C (`sdk/include/dk/fx.h`); the `[[fx]]` and `[[fx.knob]]` names below are the fields of that record.

An `[[fx]]` with a `tab` has a page in the FX view; the runtime builds the page and its knob rows from the module's records, with no effect-specific code (finding 61,
`docs/design/digikit-registry.md` "Effect pages"; the cookbook is `.agents/skills/digikit-module/references/contract-fx.md`, "Define an effect with its own page").

| Manifest key | Meaning | Limit |
| --- | --- | --- |
| `[[fx]] tab` | the page's tab label | 1 to 3 characters |
| `[[fx]] title` | the page title (the view adds `(n/N)`); mixed case like stock, the firmware font draws a capital Q like an O | 1 to 23 characters |
| `[[fx]] mix` | default of the MIX knob on the page's level slot | 0 to 127 (default 127) |
| `[[fx]] route` | the route the effect starts on when the settings do not say: `master` (an insert on the master chain, the default) or `send` (a send bus: one amount per track, set on the FX key's second page) | `master` or `send` |
| `[[fx]] routes` | the routes the user may choose in DIGIKIT > FX ROUTES | a non-empty list of `master` and `send`, no repeat, containing `route`; left out: `["master"]`, or `["send", "master"]` when `route = "send"`; a send route needs a `tab` |
| `[[fx.knob]] name`, `short` | the long name shown while the knob turns, the label under it | 23 and 6 characters |
| `[[fx.knob]] min`, `max`, `default` | the float range the effect gets, and the default (also the word's value at load) | `min < max`, the default inside |
| `[[fx.knob]] display` | `percent` (0 .. 127) or `bipolar` (-64 .. +63) | |
| `[[fx.knob]] position` | the place on the page, 0 to 7 (default: the knob's place in the list) | distinct |

Knob i is the parameter word p_i of the effect's row (7 knobs, p0 to p6); MIX is not a word: it is the adapter's dry/wet blend of the row (`master = dry + mix * (wet - dry)`, 0 is the dry signal bit for bit).
The page lives with the effect row (3 on the Digitakt II); its value indices in the kit, the page numbers and the template rows are device facts (`[fx]` of the device file), its patch-time
resources are `core/fxpages`. The packer writes the page as a `register` record of kind 4 (EFFECT) next to the effect's DSP record. The values are saved with the kit with the effect's ID (`register::effect_id`: module id and effect id): Load Kit gives an effect its stored values only when the ID in its row is its own, else the manifest's defaults. Not supported: p-locks, modulation, MIDI CC.

## 5. The adapter

An adapter is one DSP object per device and per type (`adapter-machine/src/dsp/dt2.s`, `dn2.s`) plus generated constants. It replaces `dsp-voice.s` and
`dsp-voice-dn2.s`; the hook sites and anchors stay (`module.toml` of dsp-voice :44-52, :141-211). It does the work that each `pluck_*.s` prologue does today
(section 1.3), once, for all modules.

### 5.1 Mapping (engine layout -> contract)

| Contract item | Digitakt II adapter | Digitone II adapter |
| --- | --- | --- |
| voice | track t = even voice 2t; the odd voice copies the even block | slot v |
| `state(v)` | `row.state_base + t * stride` | `row.state_base + v * stride` |
| `pitch` | frame hw `1 + t` + (TUNE word - 0x4000) when the module has a pitch parameter, else + 0 | frame hw `1 + v` |
| `velocity` | frame hw `0x1a + t` | frame hw `26 + v` |
| `params[i]` | knob word at `0x6d + 48 t + slot(i) + 2 (slot >> 2)`, `slot(i)` from the profile | `0x6d + 0x49 v + (25 + slot(i) - 25)` |
| note-on | record `+0x1b8 != 0 and +0x1b9 == 0`; the adapter writes the mark | rising edge of bit v of hw 0x11; the edge block outputs zeros, the next block runs `note_on` and `render` (this is what `pluck_dn2.s` does today) |
| `DK_DONE` | adapter writes ACTIVE = 0, clears the mark | adapter stops calling and writes zeros until the next edge |
| idle | not called (today pluck's `_pk_idle`) | not called |
| engine stages | after the adapter | after the adapter |

All positions come from the device profile (5.2); no number is in the module. The record offsets (+0x1b8, +0x1b9, 0x1d8) are firmware facts and go in
`[adapter.machine]` of the profile, with the frame bases that already exist in `[modules.frame]` (`digitakt-ii.toml:298-303`, `digitone-ii.toml:289-293`).

### 5.2 Device profile fields (proposal)

```toml
# devices/digitakt-ii.toml
[adapter.machine]
voices          = 16                      # tracks (DN2: 16 slots)
voice_unit      = "track"                 # "track": record + pairs; "slot": trig-edge
pitch_hw        = 1                       # + voice
velocity_hw     = 0x1a
type_source     = "cache"                 # DN2: "frame", hw 74
record_active   = 0x1b8                   # DT2 only
record_mark     = 0x1b9
note_on         = "record"                # DN2: "trig-edge", trig_hw = 0x11
pair            = "copy-even"             # DN2: "none"
stock_knobs     = { pitch = 0xca, level = 0xd1 }   # pitch: the TUNE knob, folded into `pitch`; level: the engine's
param_slots     = [1, 2, 4, 5]            # knob positions available to module parameters, in order (DN2: [0, 1, 2, 3])
param_template  = { percent = 0xd1, bipolar = 0xd1, enum = 0xd1 }   # DN2: 0xd5
block_cycles    = 666666                  # dsp_clock_hz / dsp_blocks_per_second (existing [board] fields)
```

`param_slots = [1,2,4,5]` keeps pluck's present knob positions (`pluck/module.toml:42`: knobs 3 and 6 empty), because a saved project stores values by
slot. Knob D (slot 3) is the sample slot on a SAMPLE clone (finding 02). Knob G (slot 6) is used by stock sample machines and works for a module; knob C is
unused by every stock machine ([finding 100](../findings/100-module-limits.md)). A module with more
parameters than `param_slots` is refused at build time with the profile's number in the message. (Slot rule: D1, section 11.5.)

### 5.3 The guard inside the adapter

The guard of finding 51 moves into the adapter unchanged in behaviour and generalises:

| Guard step | DT2 today | In the contract adapter |
| --- | --- | --- |
| clamp `params` to the declared range | clamp in place in the frame copy, put back (`dsp-voice.s:745-806`) | the adapter builds `params[]` in its own buffer from the clamped words; no write to the frame, no put-back (cheaper: about 270 of the 414 added instructions today, `51-pluck-tune-guard.md:80`) **[O]** until measured |
| output check | integer max over 32 samples against `0x41000000` (`:898-935`) | the same, on both devices |
| fault action | zero block, ACTIVE = 0, count | zero block, end voice, **reset** (adapter re-zeroes `state_clear_bytes` on the next note-on, which the contract already guarantees), count |
| disable | 8 faults in 1500 blocks, `dis` in the row | the same, on both devices |
| report | `dl_diag` words 17-23, log events 27 and 28 | the same |

The Digitone II lacked a guard because a slot had no record to end and no known reset (`51-pluck-tune-guard.md:64-66`). The contract supplies both: the adapter ends a
voice by not calling it, and resets by clearing the module's declared state. The clamp ranges for DN2 come from the declared `[[param]]` ranges and `param_slots`, not
from `voice_ranges` of machine rows (`typed.rs:564`).

### 5.4 Calling convention and cost

The adapter enters the module as the public compiler's code is entered, so assembly and C modules share one entry: `DK_ENTER` and `DK_RETURN` in assembly are
the frame instructions, a C function has them from the compiler. Finding 20 (`20-patch-toolchains.md:71-82`) measured the compiler's convention (arguments R4/R8/R12,
result R0, ADI frames) and says patchkit must supply the frame for a top-level call. This costs a few instructions per call against `JUMP (M13, I12)` today.
If the frame cannot be supplied cheaply, the fallback is today's `JUMP (M13, I12)` / `JUMP (M13, I13)` entry for assembly only, and C modules get a generated 10-line
assembly trampoline per module. **[O]**

Cost targets (instructions per active registered voice-block; measured values from finding 51 section 4 in brackets):

| Call | Today | Target |
| --- | --- | --- |
| pluck steady block, DT2 | 1776 | at most 1776 + 60 |
| pluck note-on block, DT2 | 7722 | at most 7722 + 60 |
| idle (voice not active) | 690 | at most 100 (not called; the adapter tests one bit) |
| stock type fall-through | 69 | 69, unchanged |
| DN2 guard | none | +300 per active slot (same loops as DT2) |

## 6. Runtime: module table and memory

### 6.1 One table

Proposal: replace `State.modules`, `DKRT_DSP.mod_*` and the registry's per-module bookkeeping by one record per loaded module, owned by the loader.

| Field | Meaning |
| --- | --- |
| `id`, `version`, `store path`, `sha256` | from the `.dkm` header (`digikit-dkm.md:23-49`) |
| `types` | bitmask: machine, fx, service (a module may be several) |
| `cf`: `[lo, hi)`, `bss` | ColdFire arena block |
| `dsp`: `[lo, hi)` | DSP L2 range: code, constants, state (section 6.3) |
| `rows` | machine types, parameter rows, page rows, voice rows, slot mask: each owned by exactly this record |
| `desc` | copy of the module's `dk_machine_desc` (voices, state bytes, declared cycles) |
| `health` | state (loading, live, faulted, disabled), faults, blocks since last fault, last result code |

Invariants, checked by `table_check()` after each load and by a unit test that runs on random load orders and failed loads:

1. every `cf` range lies inside the arena; every `dsp` range inside the DSP area;
2. all ranges are pairwise disjoint (CF with CF, DSP with DSP);
3. every row (type, parameter, page, voice, slot) has exactly one owner, and a free row has none;
4. the sum of declared worst-case cycles of all live machines is within `block_cycles * budget_pct` (the rule of `pk_conform.budget`, `pk_conform.py:13-23`), reported, not enforced
   (cycles are declared, not measured on a device; **[O]**);
5. a failed load leaves no range, row or slot behind (true today: `digikit-runtime.md:110-115`; the test makes it permanent).

### 6.2 Limits from the device profile

Today the limits are constants in several places: `MAX_MODULES` 8, `ARENA_BYTES` 512 KiB in `lib.rs:36-39`, types 16-23 in `types.toml:20-22`, `dsp_area_bytes` in the profile.
(Per type: D8, section 11.5.) Proposal: `[limits]` in `devices/<dev>.toml`, generated by `patchkit gen` into the runtime's `device.rs` like the other `[digikit]` fields
(`device-data.md:10-26`):

| Limit | Source today | In the profile |
| --- | --- | --- |
| machine types (run time) | `types.toml` `dynamic_first..last` (8) | `[limits] machines = 8` (derived rows stay in `types.toml`) |
| modules | `MAX_MODULES` | `modules = 8` |
| ColdFire arena, DSP area | `ARENA_BYTES`; `dsp_area_bytes` | `cf_arena_bytes`, `dsp_area_bytes` |
| voices per machine | the wrapper's 16 | `[adapter.machine] voices` |
| state bytes per machine (voices x stride) | none | `dsp_state_max` |
| run-time FX, services | none | `fx = 0` until FX exists |

The runtime is one image for all devices (`device-data.md:46-49`); a limit that differs by device must be read from the device object, as `dkrt_device_id` is. `gen` already
refuses a shared constant that differs, which tells the author what to do.

Built (docs/findings/57-capacity-isolation.md): `[limits]` also has `fx_slots`, `param_rows` and `page_rows`; `sdk/sdk.toml` `[limits.max]` holds the contract maxima and `patchkit gen` refuses a device value above
them. The runtime still reads `modules`, `machine_rows`, `arena_bytes` and `stage_bytes` as shared constants; `modules` can be lower than the selection record's 8 paths only if `E_TABLE_FULL` is reachable (it is not today).

### 6.3 DSP memory allocated by the runtime at load: feasible today

It is already how run-time DSP modules work. The code holds no address; it is linked at the nominal base `0x20000000` and relocated by the ColdFire runtime before the
send, so several modules coexist in `_dl_area` (`digikit-dkm.md:83-86`); `dsp::reserve` takes a range by first fit (`dsp.rs:260-285, 474`). The repository's DSP objects need only two relocation kinds,
`abs24` and `abs32` (`digikit-dkm.md:84-86`). `.PKBSS` follows the sent bytes in the reserved range (`:116-117`). What is **not** run-time allocated is the patched-in kind: patchkit
places `dsp.l2` objects at patch time in free L2 (`dsp_l2_excludes`, `digitakt-ii.toml:119`). After migration no content module is patched in, so the issue disappears.

What changes with the contract:

| Change | Why | Cost |
| --- | --- | --- |
| module declares `state_bytes` x `voices`; the runtime reserves `image + constants + state` in one range (state last, 64-byte aligned) | the module no longer has absolute `.PKDATA`/`.PKBSS` for its state; today's pluck state rows and lines are absolute symbols (`pluck.s:431-432`) | DSP section version 3: `voice` record gets `state_off`; the packer computes it from `dk_machine_desc` |
| voice row carries `state_base` and `stride | clear << 16` | the adapter computes `state(v)` | the row has two spare words (14, 15: `module.toml` of dsp-voice :130-136); no row growth |
| a module is refused if it has a writable absolute data symbol outside `state` | otherwise two copies of a module, or a reloaded module, share memory | packer check (section 8) |
| no `.PKBSS` or other writable static in v1 | a static shared by all voices breaks the per-voice state rule, and the public compiler puts C statics in bss | refused by the packer; a later `scratch_bytes` field can allow it |

DN2 area is 320 KiB (`digitone-ii.toml:88`): pluck's 256 KiB of delay lines are 78 % of it, so one pluck fits and a second module with real state does not. The contract makes this visible
(`desc.state_bytes x voices` in LIST) and a candidate for tuning: pluck's lines are 4096 samples per voice, 16 voices; fewer voices or a shorter line is a module choice that the limit forces
into the open. DN2 needs per-device `voices` in `[limits]` if the memory is to be shared by several machines.

### 6.4 What INFO and LIST report

INFO (`upload.rs:470-493`, `digikit-upload.md:61`) keeps its layout (commands are only added; protocol 1). Proposal: a new command `MODULES` (index -> id, version, types, cf range,
dsp range, state bytes, voices, health state, faults) and the free/largest figures for both memories; LIST keeps listing store files with `size` and `sha256`. INFO's flags gain "a module is disabled".
`dkrt_module` (`lib.rs:671`) returns the same record through the C ABI so the DIGIKIT menu can show it (`digikit-runtime.md:58`).

### 6.5 What fits (built, docs/plans/what-fits.md)

A module does not say what a load takes: the needs are inferred from the file's contents by one function (`dkneeds::needs_of`, docs/design/digikit-dkm.md, "Needs are inferred from the contents"), so a contributed or hand-built `.dkm` cannot misreport them;
the runtime checks the same limits at load anyway. The menu runs it on the card's files, patchkit and its wasm on the host (`patchkit needs`, `pk_needs`). One rule, `fits_now(needs, capacity)`, compares them with the device's CAPACITY and names the first short resource
(`dkrt_fit` for the menu; the host app calls the same code through `pk_fit` of patchkit.wasm and the tools through `patchkit fit --json -`; the vectors are `sdk/vectors/fit-vectors.json`). The DIGIKIT menu's MODULES rows show the memory a module takes and `x` when it will not fit
next to the ticked ones; the line above APPLY & RESTART gives the DSP and the CPU memory of the ticked set (`DSP 85% CPU 1%`); the Transfer page tags each library module `fits` or `won't fit` and says whether the ticked set fits together; the Memory panel of the desktop app (Emulator and Remote pages) shows what each loaded module takes of each resource the device limits; `dk run` refuses a module that does not fit unless `--force`; `module:check` prints the needs
and whether the module fits an empty device. Because the module set is fixed per session, a selection is checked against the limits with the other ticked modules taken out, not against what is loaded now.

## 7. The manifest

Author concepts only. Firmware codes move to the device profile.

```toml
# modules/machines/pluck/module.toml
[module]
id      = "digikit/pluck"
name    = "Pluck"
short   = "PLK"
type    = "machine"                     # contract: machine v1
version = "0.2.0"
summary = "A plucked-string synth machine."
devices = ["dt2", "dn2"]                # which profiles have been conformance-checked; firmware ports come from the profiles

[machine]
pitched = true                          # uses the pitch (adds the device's tune knob where the profile has one)
voices  = 16
source  = { dsp = "src/dsp/pluck.s" }   # the one routine for every device; per-device override: [machine.source.dn2]; one compiled module for all devices: D4, section 11.5

[[param]]
id = "decay"   name = "Decay"           short = "DECAY"  kind = "percent" min = 0 max = 0x7f00 default = 0x4000
[[param]]
id = "pick"    name = "Pick position"   short = "PICK"   kind = "percent" min = 0 max = 0x7f00 default = 0x2000
[[param]]
id = "bright"  name = "Brightness"      short = "BRIGHT" kind = "percent" min = 0 max = 0x7f00 default = 0x5000
[[param]]
id = "color"   name = "Excitation colour" short = "COLOR" kind = "percent" min = 0 max = 0x7f00 default = 0x2800
# order of the [[param]] tables = knob order. `knobs = [...]` exists only to reorder: knobs = ["decay", "pick", "bright", "color"]. (slot rule: D1, section 11.5)

[budget]
block_instructions   = 2200             # the author's claim; the rig must stay below it; both must stay below the device headroom / voices
note_on_instructions = 8000
```

What disappears from the manifest, and where it goes:

| Today (`pluck/module.toml`) | Moves to |
| --- | --- |
| `knobs = [0xca, ..., 0xd1]`, `template = 0xd1`, `[param.dn2] template = 0xd5` | profile `stock_knobs`, `param_template` (5.2); `kind` selects the template |
| `clone_of = 0`, `[machine.dn2] position = 4, own_dsp = true`, `[[page]]` | profile `[adapter.machine.rows]`: `clone_of`, `position`, `own_dsp`, page naming rule (page name = machine short name, title = name) |
| `[[voice]] machine, entry = "_pk_render"` | the fixed symbols `dk_machine_note_on`, `dk_machine_render`, `dk_machine_desc` |
| `place = "dsp.l2"`, `[object.externs]`, anchor rules | framework only; a content module has none |
| `ports = [...]`, `[targets]` | generated from the profiles that pass `module:check` (the test status table stays, as generated output) |
| type number in `types.toml` | static machines: stays in `types.toml`, key `<module id>:<machine id>`, allocated by `module:new`, never by hand. Run-time machines: no entry; the runtime assigns the number at load and the card's sticky map keeps it (D7, section 11.5, implemented) |

One schema, written once in Rust (`packages/patchkit/src/modschema.rs`, extending `typed.rs`), generated into JSON Schema for editors and into the authoring guide. Errors name the
file, the key path, the rule and the fix, and are produced by the same code in the CLI and in `patchkit.wasm`:

```
modules/machines/pluck/module.toml: [[param]] "decay": max 0x8000 is above the largest word 0x7f00 for kind "percent".
modules/machines/pluck/module.toml: 5 parameters declared; digitakt-ii allows 4 ([adapter.machine] param_slots in devices/digitakt-ii.toml).
```

Generated constants: `build/gen/` per module as today (`consts.rs`, `<dev>.inc`, `gallery.json`; finding 27, `27-module-schema.md:20-21`), plus `desc.inc` (`n_params`, ranges) that the
assembly can `#include` to check its own assumptions at assembly time (a `.if` on the parameter count).

## 8. Checks: build, pack, load

| Check | Today | Moves to | How |
| --- | --- | --- | --- |
| entry symbols present, `dk_machine_desc` valid (magic, contract version, `n_params` = manifest, sizes multiples of 4) | pluck's `_pk_render` symbol is named in the manifest; a missing symbol fails at link | build (pk_build) | read the object's symbol table and desc bytes |
| ISA: every instruction decodes | build | build | unchanged (`pk_build.py:9-11`) |
| register rules | the rig, on executed paths only | build, static | decode all instructions and compute the written-register set: no write to the "must keep" set; C build: I6/I7 only via frame instructions. Indirect jumps and `CALL (reg)` are refused |
| call depth | none | build, static | call graph from the object; indirect calls refused; `max_call_depth` must equal or exceed it |
| writable absolute data | none | build | a store whose address comes from a data relocation outside the `state` argument path is hard to prove; v1 rule: no `.PKDATA`/`.PKBSS`/`.VAR` writable symbol at all (constants are read-only), and the rig's write-set check below |
| writes only own state | the rig, `tests/support/machine_rig.py` (`record_stores`, `_allowed`) | conformance rig | **exists**: the concrete core logs every store through the driver's write watchpoints (`sharc_run.Watchpoint`, switched on per call by `pk_sharc.CallSession`); a store by the module's code outside this call's output block, the calling voice's `state_bytes`, the module stack (`stack_bytes` below the adapter's frame) and the module's own data and bss fails (that data is shared by all voices: state that must differ per voice goes in `state`) (group `writes` adds two voices at once; negative fixture `test/dyn-guard-nan#overrun`). The `dk_voice_in` and `params` copies are the adapter's data and are read-only for the module |
| worst-case cycles | a sample of cases | rig, with declaration | `max_cycles_block` declared; the rig's observed maximum must be at most the declared one and at most the device budget; adversarial cases stay (limits, modulation, retrig: `pk_conform.py:48-53`) |
| parameter ranges | partial | build | `min <= default <= max`, inside the kind's range, `n_params <= param_slots` for each target device |
| state size | none | build | `state_bytes x voices` fits `[limits] dsp_state_max` of each device |
| device list | `ports` by hand | build | every listed device passes the rig |
| pack: one CF + DSP object, relocation kinds, imports | pack | pack | unchanged (`digikit-dkm.md:57-62`) |
| load: sha, format, runtime version, imports, device, space, rows, types | load | load | unchanged order (`digikit-dkm.md:131-147`); no write before the last check |

`.dkm` header fields the runtime checks, with new ones marked: `runtime_min` (raise to 6 for contract modules; compatibility: D3, section 11.5), module version, name, **`types` bitmask** (the loader refuses a type the
profile has no limit for), **`contract`** (the highest contract version per type; the runtime lists the versions it supports), DSP section version 3 with `voices`, `state_off` and
`desc` (copied into the table), and the existing device word, bounds and relocation checks. An older runtime refuses version 3 with `E_DSP_FORMAT`; the old and new loaders never mix because
of `runtime_min` (compatibility: D3, section 11.5). The sha256 stays an integrity check, not authentication (`digikit-dkm.md:159-160`).

Run-time checks that stay because they cannot be made at build time: clamp (kept, against the host calling with out-of-range words from an LFO), output finite and bounded, fault count and disable, the
`entered[]` counter for ColdFire slots, the card-safety gates.

## 9. Authoring flow and tooling

```
mise run module:new machine NAME      create modules/machines/NAME from the template: module.toml, src/dsp/NAME.s with the three symbols and a silent render,
                                      tests/test_NAME.py (sound tests), gallery/icon.svg, a type number in types.toml, builds it
mise run module:check NAME            build (pk_build), static checks (section 8), conformance on every device in `devices`, then the module's own tests, the packed size, the inferred needs and whether the module fits an empty device (6.5)
mise run module:pack NAME [--device]  .dkm (what `patchkit pack` does)
dk run NAME                           build, pack, check that it fits (6.5; `--force` uploads a module that does not), upload, select, restart a device or the emulator, wait for the boot, report the log (tools/device/dkrun.py)
mise run module:emu NAME              the same through the emulator Target (Phase 11 item 6)
```

| Step | State today | Change |
| --- | --- | --- |
| `module:new` | `patch:new` makes a module from a template (`mise.toml:275-277`) with category/name, no types | one template per type; `machine` first |
| `module:check` | exists for machines (`pk_modcheck.py`, `mise.toml:98-100`) | add the static checks of section 8; non-machine modules get their type's checks |
| emulator run | `tests/digikit` scenarios; `menu-dkrun-pluck` drives `dk run` in the emulator | the Target abstraction (Phase 11 item 6, `plans/digikit-runtime.md:455`): emulator and device backends behind one API, so one system scenario runs on both |
| device run | `tools/device/dkrun.py`, INFO, PUT, SELECT, RESTART, LOG (`digikit-upload.md:165-166`) | unchanged; the guard's `MACHINE_FAULT` events become a pass/fail line in the report |
| guide | `docs/MODULE-AUTHORING.md` (Phase 11 item 5) | one page: the contract header, the manifest, four commands, a worked example (pluck), the failure messages and what they mean. It is generated in part from `<dk/machine.h>` and the schema, so it cannot drift. Plain, short, with tables: written for a human and for a model reading it cold |
| C modules | not supported on SHARC (finding 20 recommends assembly for the hot path) | allowed by the contract; `module:new machine NAME --lang c` is offered only after the measurement in section 11 |
| ColdFire side | Rust (`rust-demo`) and C (`c-demo`) | the `dkrt-sdk` crate stays the SDK of the service type |

The SDK a module author sees: `<dk/machine.h>` (or `sdk/asm/dk_machine.inc`), the template, the four commands, the guide. No firmware knowledge, no hook, no frame offset.

## 10. Migration

Every step keeps `main` green (`mise run verify`), is one lane, and starts from a recorded oracle. PCM pins in `tests/scenarios/*.json` change only with Em's approval (AGENTS.md):
a step that would move a pin stops and asks.

### 10.1 Order

| Step | Content | Oracle |
| --- | --- | --- |
| 0. Record | with the current tree, record on the rig and in the emulator what the later steps compare against: float32 samples of `MachineRig` pluck for DT2 and DN2 on the conformance cases plus the finding-51 grid (every note x six TUNE words, 200 blocks each: `51-pluck-tune-guard.md:30-34`), and the PCM hashes of `equiv-dyn-pluck`, `menu-dkrun-pluck`, `pages-dyn-pluck`, `debug-guard`, `rung-c` | files under `out/` (not committed), plus the existing PCM pins |
| 1. Contract and generator | `sdk/dk_machine.h`, `pk_sdkgen.py`, `dk_machine.inc/.rs`; no behaviour change | a unit test round-trips the header into the two outputs; ruff, mypy |
| 2. DT2 adapter v2 + pluck | adapter calls `dk_machine_*`; pluck gets the new entry points and one source; state in rows; guard moved into the adapter | see 10.2 |
| 3. DN2 adapter | same for DN2 incl. guard and reset; pluck's DN2 source is deleted (same source) | rig PCM equal to step 0 for DN2; `equiv-dn2-set`, `pages-dyn-pluck`, `roundtrip-dn2` |
| 4. Runtime table, limits, state placement | section 6; DSP section v3 | `table_check` unit test; `tests/digikit` boot scenarios (`card-safety`, `crash-capture`, `safe-fault`, `debug-*`) unchanged results; a new test that fills the machine rows, the DSP area and the module table and expects the named error codes |
| 5. Manifest v2 and profile fields | section 7 | pluck rebuilt from the v2 manifest produces rows equal to today's `plan.json` rows for the same firmware (the test of finding 27, `27-module-schema.md:23-40`, with the new inputs); `patchkit gen --check` |
| 6. Build-time checks | section 8 | each check has a negative fixture that must fail with the named message (as `pk_conform.NEGATIVE`, `:58-61`) |
| 7. Fixtures | `dyn-guard-nan`, `dyn-dsp-hang`, `dyn-fault`, `dyn-fault-late`, `dyn-crash`, `c-demo`, `rust-demo`, `dn2-*`, `twin-*`, `registry-demo`, `dyn-demo`, `dyn-params`, `dyn-sine-dn2` move to `tests/fixtures/modules/`; scenarios and `types.toml` keys are renamed | the scenarios' PCM and log results unchanged |
| 8. Retire patched-in content | delete `digikit/pluck` as patched-in and `test/dyn-pluck`; one `digikit/pluck`, always run time; same for `xslice` if it fits a contract (`master-gain` is deleted) | see 10.3 |
| 9. Tooling and guide | `module:new`, `module:check` extensions, Target, the guide | `module:new machine demo` then `module:check demo` passes on both devices, in CI |

### 10.2 Pluck first: what is compared

Pluck is one module with one routine for both devices if the kernel (lines 100-405 of both files) is word-identical after the contract: it is, except lines 220 and 267-272 **[D]**; step 2 must
read those hunks and say whether they are ABI (record versus slot) or algorithm. Expected result: yes, ABI.

Byte-identical images are not expected: the framework object changes (adapter), the `.dkm` of pluck changes (new entry points, desc, state in rows). What is compared, exactly:

| Compared | Expected |
| --- | --- |
| rig output, DT2 and DN2, new pluck through the new adapter vs step-0 record: the 32 samples of every block, bit for bit, on the conformance cases and the finding-51 grid | equal, including the clamped corner cases (the clamps move into the adapter and into pluck unchanged) |
| the three clamps of finding 51 are still in pluck, and the adapter's clamp range equals the manifest range | equal |
| `equiv-dyn-pluck`, `menu-dkrun-pluck`, `pages-dyn-pluck`, `debug-guard` PCM and event logs | equal PCM hash; `debug-guard` (both images since P3b-2): `MACHINE_FAULT` of the fixture's assigned type, count 8, `MACHINE_DISABLED` once, as in finding 51 section 5 |
| stock path: the seven DN2 pins and the 61 DT2/DN2 scenario oracles with the framework and no content module | unchanged (stock behaviour) |
| conformance on both devices; `max_cycles_block` observed at most the declared | pass |
| cost table of 5.4 | within target |
| `.dkm` DSP kernel bytes: relocation-masked bytes of lines 100-405 equal to today's | equal; a difference is explained in the commit |

The new guard test: a fixture that outputs NaN and one that leaves an out-of-range parameter reach it through the adapter on **both** devices (today DT2 only).

### 10.3 Retiring the patched-in content modules

Patched-in `test/patched-pluck` (before 2026-10-21: `digikit/pluck`) has type 9; the run-time one has 18 (DT2) and 17 (DN2) (`types.toml:27,41,54,67`). A project saved with type 9 would load as a gap row that plays as type 0
(`digikit-registry.md:99`). Options: (a) accept, if no saved project uses type 9 (Em has the only device); (b) keep type 9 reserved as an alias that the loader maps to the run-time pluck (needs a
firmware-side map; not built). Recommendation: (a), after Em confirms. **[O]**

`xslice` clones SLICE and needs the sample renderer, not a synthesized voice: it is not a Machine-contract module. It either stays patched in as a framework-adjacent module until a "sample machine"
type exists, or it is dropped from the SDK scope. **[C]** `digikit/master-gain` and `master-drive` are deleted (Em, 2026-10-29): the devices have master overdrive, and `digikit/freqshift` is the example of an effect on the master route.

### 10.4 Committed `.dko.json` versus CI-built

| | Keep committed | Built in CI only |
| --- | --- | --- |
| browser patching of framework modules | works with no toolchain (`patchkit.wasm` reads the objects) | the Pages build needs Selache, m68k gcc and rustc_codegen_gcc (about 1 h cold: `pk_toolchains.py:14-18`); a local clone cannot patch without them |
| review | diffs of JSON objects (the runtime object is 388 KB) | clean diffs |
| drift | `pk_build --check` fails on a stale object (`pk_build.py:14-16`) | cannot drift; the artifact is rebuilt |
| reproducibility | the object holds the source hash | needs pinned toolchains (already pinned: `pk_toolchains.py:39-60`) |

Recommendation: framework objects stay committed (few, change with firmware ports, needed by the browser). Content modules are not committed as objects: they are built by CI into `.dkm` files
(small, hashed, no firmware bytes) published with the site, and the browser copies a `.dkm` into the store; no browser linking of content is needed because a content module no longer has a patch-time form. A local author builds
with the toolchains (`module:check` needs only Selache, no firmware). If the web app should also let a user *edit* a module in the browser, that needs the toolchain in wasm, which does not exist: out of scope.

## 11. Risks, open questions, estimates

### 11.1 Risks

| Risk | Detail | Mitigation |
| --- | --- | --- |
| C on SHARC | the public compiler was evaluated on six small functions; no optimiser to speak of (24 instructions against 7 by hand for a scale loop); pointers are byte addresses, so whether silicon agrees is open (`20-patch-toolchains.md:71-82`); the frame call convention | assembly remains the supported language of v1; a C pilot (the `pluck` string loop in C, measured against the assembly on the rig for cycles) before `--lang c` is offered |
| contract cost | adapter and frame call add cycles; DT2 has 16 tracks x 666,667 (1 GHz, [D] finding 100) cycles per block of headroom shared with the firmware | cost targets in 5.4, measured on the rig in step 2 before step 3 starts |
| DN2 memory | 320 KiB DSP area; pluck alone uses 256 KiB | state is declared and reported; `voices` per module and per device is in the profile |
| DN2 engine constraints | the note-on is a one-block trig bit; the amp envelope and gate come after the module; whether the engine reads `own_dsp` machines the same for every module is only read for pluck (finding 33) | the DN2 adapter is the only place that knows; step 3 re-runs `equiv-dn2-set` and `pages-dyn-pluck` |
| "write only own state" is not provable statically | computed pointers | write-set check on the rig (needs a store hook) plus a canary word after each state block, checked by the adapter, cost about 4 instructions per call |
| the ColdFire side is unprotected | a Rust or C service module can write anywhere | not addressed; the arena and the write watch (`digikit-runtime.md:114`) catch only emulator-visible cases |
| contract churn | a field added to `dk_machine_desc` after modules exist | `contract` version per type; the adapter supports the last two; fields only appended |
| project compatibility | type numbers, knob slots and values are stored in projects | permanent numbers stay; `param_slots` keep pluck's positions (5.2) |

### 11.2 Questions for Em

1. Is a saved project with pluck as type 9 (the patched-in one) in use? If not, retire it without an alias (10.3).
2. C on SHARC: do you want the pilot measured before the contract is frozen, or contract first with assembly only? Recommendation: contract first; the contract does not depend on the language.
3. DN2 DSP memory: accept 256 KiB for one pluck, or shorten the lines (changes the sound at low notes) or reduce `voices` on DN2?
4. Should `xslice` (a SLICE clone, no synthesis) be part of the SDK as a second Machine kind ("sample machine") or stay outside?
5. Cycle enforcement: SHARC cannot be preempted; only the guard's output check and the DSP hang breadcrumbs (finding 48) exist. Is a declared-and-measured budget enough, or should the runtime take the DSP block time from HEALTH and disable the heaviest machine when the block time is over budget?
6. A new command `MODULES` and DSP section v3 raise `RUNTIME_VERSION` to 6; a card with runtime-5 `.dkm` files must still load (the plan: yes, both loaders in one runtime). Confirm. (Superseded by D3, section 11.5.)

### 11.3 Estimates (agent time, a lane each; wall time is shorter with lanes in parallel; outcome risk stated separately)

| Step | Agent hours | Outcome risk |
| --- | --- | --- |
| 0 record oracle | 1-2 | low |
| 1 contract header and generator | 3-5 | low |
| 2 DT2 adapter v2 and pluck | 8-12 | medium: cost and exact PCM equality |
| 3 DN2 adapter with guard and reset | 8-12 | medium-high: engine details read only for pluck |
| 4 runtime table, limits, DSP v3, MODULES | 8-12 (the runtime object rebuilds fast; the generated SHARC core does not change) | medium |
| 5 manifest v2, profile fields, errors, wasm | 6-9 | low |
| 6 build-time checks (static register/call-depth scan, write-set hook) | 6-10 | medium: the store hook on the core |
| 7 fixtures | 3-5 | low |
| 8 retire patched-in, CI `.dkm` | 3-5 | low |
| 9 tooling and guide | 4-6 | low |
| C pilot (optional) | 4-8 | high that quality is poor |

Revised: section 11.5. Total about 50-80 agent hours; 3 to 6 days of wall time with two or three lanes. Payoff: one pluck instead of four files and two routines, every later machine written once for two
devices, a guard on the Digitone II, a table that proves memory is not shared, and an SDK a model can use from one header and one guide. Confidence that the contract carries pluck: high (the kernel is shared).
Confidence that it carries a machine that needs the engine's own stages (filters, `xslice`): low, and out of scope for v1.

### 11.4 Answers (Em, 2026-10-19)

1. No saved project uses the patched-in pluck (type 9): retire it with no alias. **Later:** whatever a project persists that names a
   module (machine types, FX, others; to be checked) must be matched against the loaded modules on project load: report a missing or
   different module, later offer to load it.
2. Knobs 3 and 6 of the Digitakt II pluck: find out during migration whether they can hold machine parameters.
3. Assembly only for v1; the header stays C-style so a C routine can implement it later (a measured pilot first).
4. Digitone II DSP memory: decided when the Digitone II is migrated (shorter lines, fewer voices, or state per playing voice).
5. XSLICE is a demonstration of the clone mechanism, not a user module: it becomes a framework fixture; no clone kind in the SDK for now.
6. Budgets are declared and measured (conformance); the runtime logs when the DSP's block time goes over; no automatic disabling yet.
7. The SDK and the `.dkm` format are unstable for now: a runtime refuses a module built for another runtime version (dk run rebuilds);
   no compatibility layer. (Superseded by D3, section 11.5.)
8. Hot reload's swap policy (click or wait for stop) is framework only; at most an optional `release` entry in the contract later.
9. Logging is over USB only. Log levels are set from the Mac per session and not stored on the card (no card write for them).
10. The voice row grows to 24 words in the first SDK version (state placement and the generation counter in one format change).

### 11.5 Decisions (Em, 2026-10-20)

These supersede earlier text where they conflict; the passages they change point here.

- **D1. Parameters and knob slots.** The `knobs` array of `[[machine]]` (and of `[machine.dn2]`) fixes each parameter's slot: its position is the knob. There is no auto-fill and no slot file. The build refuses a module parameter on knob 3 of a DT2 SAMPLE clone (the sample slot: only the sample number bits reach the DSP, measured with and without a sample assigned: docs/findings/02-machines-and-parameters.md; the fact is `[[modules.knobs.reserved]]` in `devices/digitakt-ii.toml`), a name in `knobs` that is no `[[param]]` of the module, and a parameter named twice. Projects store parameter values by slot, so a released parameter must keep its slot: a moved parameter at export is checked against the previous `.dkmod` (P3c-5, optional). Pluck's DT2 slots stay 1, 2, 4 and 5; knob 6 is usable (xslice holds a parameter there, docs/findings/30-runtime-machines.md).
- **D2. Module types.** `[module] type = "machine" | "fx" | "service" | "raw"`. Each typed kind has its own SDK and contract version. `raw` has no contract: arbitrary code with only the generic load checks (format, SHA-256, imports, bounds), shown as unchecked in the menu; it can move to a typed SDK later. A raw SHARC routine has no guard; safe mode is the recovery. A tool with a UI page is a `service` once a UI hook exists.
- **D3. Metadata and compatibility.** `id = "author/name"`, `version` (major.minor.patch), `author`. A module records the contract version of each type it implements; the runtime refuses a module whose contract major version differs from its own. It does not refuse a module only because it was built for another runtime version. This supersedes answer 7 and the `runtime_min` rule.
- **D4. One compiled module for all devices.** A `.dkm` holds one code body for every device it lists. The device adapter is part of the runtime image, which is built per device and firmware; it carries the layout differences (voice record, knob frame, tracks against voices, guard). Module DSP code is relocatable: the runtime applies DSP relocations at load. A module declares its state size and is refused on a device where it does not fit (DN2 320 KiB, DT2 512 KiB DSP area). A per-device code section is allowed only where the algorithm itself differs. Pluck already works on DT2, so its DT2 output is the oracle; the DN2 copy is folded into the same body after its kernel hunks (section 1.3) are classified. A new device or firmware needs a new runtime image, not new modules.
- **D5. Size.** Modules and the runtime are as small as possible for a given device and firmware. The runtime image carries only that device's adapter and the services it uses. A module carries one code body and compact relocations, no per-device copies. `module:check` and `patch:mem` report the sizes, and a PR states size changes.
- **D6. Languages and containment.** ColdFire: Rust or C. SHARC: assembly in v1 (no usable SHARC C compiler: docs/findings/20-patch-toolchains.md). Neither CPU has memory protection, so the runtime's Rust does not contain module code. Containment is the build-time checks, the load checks, the adapter guard on both devices and the write-set check in the rig. **ColdFire audit implemented (P4b):** `mise run module:audit` and the `memaudit-*` scenarios arm the emulator's write guard on the free ColdFire regions outside each module's block while the UI is driven, and report any store by module or stock code (docs/TESTING.md); DMA is not covered.
- **D7. Machine ids.** A machine has a global id `author/name:machine`. The card keeps a sticky map from global id to run-time type number (16 to 23) in the module store: a machine keeps its number while it stays installed, so projects and presets, which store the type byte, keep working. **Implemented 2026-10-20 (P3b-2):** the global id is `<module id>:<machine id>`; its hash is the low 24 bits of FNV-1a 32 of that UTF-8 string (`hash24`); the `.dkm` carries `hash24 << 8` in the machine, page and voice type fields (type byte 0 = assign at load), the runtime assigns the number at the boot load and keeps a map of 8 `hash24 << 8 | type` entries in the selection record's reserved bytes (bytes 28 to 31 and 480 to 507; eviction of the lowest entry of a machine that is not in the boot when the map is full, logged `MACHINE_TYPE_EVICTED`); `[<dev>.dynamic]` is gone from `types.toml` (`digikit-registry.md`, `digikit-store.md`). Later: when a project loads, check that its machines are installed and valid, or load them automatically. This needs a map from project to modules saved with each project, and so a project save and load hook, which is not found yet.
- **D8. Limits per type.** `[limits]` in the device profile, enforced by the runtime: 8 machine rows (types 16 to 23), 8 modules, the DSP area, the ColdFire arena. The menu shows the slots in use.
- **D9. Runtime version.** The runtime has a semantic version, packed major<<16 | minor<<8 | patch, starting at 0.1.0 (sdk/sdk.toml). Versions 1 to 5 were plain integers and read as 0.0.1 to 0.0.5. While 0.x, MINOR may break (a contract major, the card store format or the SysEx protocol) and PATCH covers features and fixes; from 1.0.0, MAJOR breaks, MINOR adds, PATCH fixes. The runtime's bump is the largest among the interfaces it carries; the adapter is versioned with the runtime image it is built into. Module compatibility is decided by the contracts (D3), not by this number.
- **D10. Manifest and ids (2026-10-20, second round).** The manifest has no schema number. `category` stays. `type` also has the internal value `framework`; `module:new` refuses it, and refuses ids under `core/`. The ids `core/` and `test/` are reserved: content is `digikit/*`.
- **D11. Export.** The export format is `.dkmod`: a deterministic zip without `build/`, plus `package.toml` (id, version, sha256 per file).
- **D12. Raw path and public module.** The old-form voice row (a row with no `note_on`) stays as the path of a `raw` module (D2): no contract, no guard clamp. The patched-in pluck stays the public browser module until the web flow delivers a `.dkm`.
- **Estimate.** With D4 (DSP relocation, one pluck body) and the steps merged into three PRs, about 30 to 45 agent hours. Most of the risk is in the DN2 adapter. The table in 11.3 is the earlier per-step upper bound.

### 11.6 Service commands (api 1.2, runtime 0.2.5)

A module that needs a SysEx command of its own (the first is `digikit/remote`: key, turn and state of the panel) calls `Api.register_cmds(first, count, handler)` from `init`.
The handler is `int handler(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)`: it returns the number of reply bytes written to `out`, or a negative status. It runs in the firmware's
MIDI task, so it is short and never waits. The ranges a module may take are reserved in `sdk/sdk.toml` `[protocol.ranges]`; a request outside them, over another module's, or empty
returns `E_CMD_RANGE` (-64). The ranges are released when the module's `fini` has run. `docs/design/digikit-upload.md` ("Service commands") has the wire side; the fixture
`test/svc-echo` and `modules/core/runtime/tests/service-commands.device.test.ts` show the round trip; the acceptance cases run with full verification, explicit `--only runtime-fixture`, or `pnpm nx:pilot run core/runtime:test-device` (see `tests/regression/README.md` for prerequisites).

### 11.7 The input adapter (`core/input`, runtime 0.2.5)

A module that wants to press a key or turn an encoder imports two functions by name from the runtime's table (a runtime without `core/input` does not have them; the load fails with
`E_IMPORT`): `int input_key(u32 code, u32 down)` and `int input_turn(u32 encoder, int delta)`. They return 0, or `-1` (not a key code, or an encoder that does not exist), `-2` (the UI
queue is full), `-3` (the device profile names no backend `core/input` has). Codes are the device file's `[panel]` codes; the backend is the device file's `[input] backend`. They call the
firmware's own panel decoders (`docs/findings/58-panel-input-path.md`) and are for the MIDI task; the first user is `digikit/remote`, the proof is `test/input-probe` and the scenario `input-adapter`.
A device that reaches its events another way adds a backend to `core/input` and names it in its profile; modules and hosts keep calling the same two functions.

### 11.8 digikit/remote

`digikit/remote` (`modules/tools/remote`, a run-time `.dkm`, the same file for both devices) registers KEY 0x38 (`code`, `down`), TURN 0x39 (`encoder`, signed `delta`; the reply is the delta
taken, at most 30 either way, and the host sends the rest again), RELEASE 0x3a (lets go of every key the module holds) in the `remote` range, and STATE 0x3e (`enabled`, always 1 while the module is loaded, format, turn
limit) in the `remote_state` range. The module knows no device: codes and encoders are the device file's `[panel]` (the host lays the keys out from them). The module is on exactly when its check mark in DIGIKIT > MODULES is set (there is no REMOTE switch and no guarded range). Its settings page (`dkm_menu`, SETTINGS... of the MODULES row) shows the keys it holds, the last command and RELEASE ALL KEYS. Proof: scenario `remote-control`, the host tests `modules/core/adapters/ui/host/tests/remote.rs`.

## 12. Hot reload as a later goal

Not in SDK v1. The aim is that v1 does not make it harder.

### 12.1 History

| Item | Fact |
| --- | --- |
| built | load and unload while the firmware runs, run-time machines on both devices, DSP code with atomic voice rows, unload refusal while a track uses the type (emulator only) (`docs/plans/digikit-hot-reload.md:8-22`) |
| commits | tag `digikit-hot-reload-v1` = `928dc1d3` (2026-10-17); switch to boot-time loading `e8b3a04f`; hot-only code removed by `a12048c4` "digikit runtime: archive hot reload" (2026-10-17); `git revert a12048c4` restores it (`digikit-hot-reload.md:3-5, 24`) |
| what was removed | `dkrt_unload`, the in-use scan, the machine-view rebuild, the load queue, the run-time DSP load path, four `core/dynreg` anchors and the view hook (`:24-47`) |
| why set aside | decision 9 (`plans/digikit-runtime.md:41-47`): a load under an open, playing project needs a transport guard, an in-use scan of the whole project, write ordering under concurrent readers and a soak; a restart removes the class. Em agreed; "may return later, first for code-only modules" |
| still needed | the seven items of the 2026-10-17 risk review (`digikit-hot-reload.md:49-62`) |

### 12.2 Constraints the SDK keeps now

| Constraint | Why hot reload needs it | Today / v1 | Cost now |
| --- | --- | --- | --- |
| Calls into a module only through the runtime's row or slot (DSP voice row `pc`, ColdFire slot pointer); no patched call, no stored address of module code outside the table | a swap changes one pointer | true: `dsp-voice.s:103-110` scans the row; slots are published by one aligned store (`digikit-runtime.md:87-91`) | none; make it a rule of section 3 |
| Firmware hooks only in the framework, through a trampoline that reads a table | a module can come and go without patching | true: one hook, `dkrt_loop_hook` (`digikit-runtime.md:39-47`) | none |
| Quiesce point: DSP: row type word set to "free" (`0xffffffff`) first, then wait for the block counter to advance twice (heartbeat, `dlog.rs:123-134`) before the range is reused; ColdFire: the UI task between events, `entered[n] == 0` | no call may be inside the old code when its memory is freed | row order exists (`digikit-dkm.md:119-123`); `entered[]` exists (`digikit-runtime.md:91-94`); the DSP wait is not written | one rule in the adapter: "a freed row is never read again after the current block"; the wait is runtime code, later |
| The row has a `disable` word | disable first, free later | exists (`dis`, finding 51) | none |
| State owned by the adapter, cleared through the contract (4.1, 5.3); a module has no statics (6.3) | a new version starts clean; the old state block is simply freed | designed | none |
| DSP memory from the runtime with free, no module holds an address of another | range reuse | first fit with release exists (`dsp.rs:260, 474`; `release`, `unreserve` kept, `digikit-hot-reload.md:41-43`) | none |
| Generation counter: each module record has `gen`, incremented on every load and unload of the record; rows and log events carry (index, gen) | a stale reader (host tool, in-flight request) sees that the module changed | no | one word in the record and in the voice row. The row has no free word after `state_base` and stride; either use the packed `stride | clear << 16` word with 8 bits of gen, or grow the row to 24 words with a row-size version in `dl` window 1 (`51-pluck-tune-guard.md:47-48`). Choose when section 6 is built; grow the row once, now, if cheap |
| Type number, machine row, parameter rows stay across a swap of the same module id | projects keep their sounds | type numbers permanent (`types.toml:16-18`); parameters keyed by slot (`plans:49-50`) | none |
| Machine-row and parameter-row writes ordered for concurrent readers | the UI task reads them while another writes | the boot path orders them (`digikit-registry.md:83-90`) | write the order down as part of `registry.rs`; no change |

### 12.3 What happens on a swap (design for later)

Same module id, new version, same type number: allocate the new DSP range and arena block; send; disable the row; wait the quiesce; publish the new `pc` and ranges (row type last); free the old range. Memory
is needed twice for a moment (pluck on Digitone II cannot do this: 256 KiB of 320 KiB). Sounding voices of the type end at the swap (ACTIVE = 0 on the Digitakt II, no more calls on the Digitone II); a held note
is not retriggered. Parameter locks and projects are unaffected (keyed by slot and type). Unload without a replacement: the row becomes a gap row (`digikit-registry.md:99`): the track plays as type 0, which is the
wrong sound; so unload keeps the old refusal ("in use") or shows a warning. Parameter-range changes between versions: the guard clamps the stored values.

Waits for later: transport guard; the whole-project in-use scan; the machine-view rebuild for a new machine (the view is built once at boot, `digikit-runtime.md:167-172`); the soak. First target as in the archive: code-only ColdFire modules, behind the transport guard.

## 13. Observability: logging for modules and emulator parity

Scope: what the SDK contract and the adapter must provide. The tools and the transport are Phase 10 items 4 to 6 (`plans/digikit-runtime.md:403-436`).

### 13.1 Today

| Item | Fact |
| --- | --- |
| log | events of 32 bytes: `seq`, block-counter `time`, `kind`, `flags` (bit 0 error), five words; a small ring of 32 in the runtime (`dlog.rs`), 512 in the module `digikit/debug` (`46-digikit-debug.md`) |
| switch | the module `digikit/debug` loaded or not; levels by LOGCFG (RAM only), no categories |
| transport | SysEx LOG 0x20, PEEK 0x21, INFO (`upload.rs:36-45`); the emulator answers the same (`dklog.py --emu`); one decoder, `tools/device/dklog.py` (`KINDS`, :111), a hand copy of `dkrt_sdk::logk` (`lib.rs:866`) |
| module logging | ColdFire only: `Api.log(msg, value)`, 24 characters, kind MOD_LOG (14), no module id (`digikit-runtime.md:82`, `46-digikit-debug.md`, table row 14) |
| DSP | no API. The wrapper writes fixed words: heartbeat, context registers, breadcrumb, dispatch and return counters, guard words, 24 words in the reply record (`dlog.rs:123-134`; `48-dsp-breadcrumbs.md:8-40`; `51:59-62`) |
| faults | MACHINE_FAULT and MACHINE_DISABLED carry a machine type, not a module (`46` kinds 27, 28); crash capture records the module of the PC (`crash.rs`) |
| other | PEEK of RAM ranges from the device profile (`[digikit] peek`, `devices/digitakt-ii.toml:102-104`), firmware screenshot, HEALTH snapshots |

### 13.2 Levels, categories, run-time control

| Item | Design (proposal) |
| --- | --- |
| levels | error, warn, info, debug, trace (0 to 4) in `flags` bits 1 to 3; bit 0 stays "error" so old readers work |
| source | `flags` bits 8 to 15: module record index + 1 (0 = the runtime); with `gen` in argument 4 where an event is module-scoped |
| categories | the existing `kind` ranges are the categories: boot, card, upload, dsp-link, adapter, machine, module, crash. A `LogConfig` of one default level, one level per category (16 bytes) and one per module record (8 bytes) |
| control | a new SysEx command `LOGCFG` (get, set): `dklog level debug`, `dklog level dsp-link=trace`, `dklog level module:pluck=debug`. No reflash |
| memory | the default level and the category levels would go in the selection record (bit 1 of its flags is reserved, once the debug-off flag); per-module levels are not remembered (modules come and go), they reset at boot. Whether the record has room for 16 bytes is **[O]** (`digikit-store.md`) |
| cost at off | one pointer test in `ev()` before any work, when no sink is set (`dlog.rs`) |

### 13.3 Module logging API

| CPU | API | Rules |
| --- | --- | --- |
| ColdFire | `Api.log_ev(level, code, a0, a1, a2)` appended to the `Api` table (fields only appended: `digikit-runtime.md:79-80`) next to the old `log` | the runtime filters by level, stamps module index and gen, applies a rate limit per module (token bucket, proposal 20 events/s, then one "N dropped" event); no allocation; callable from slot functions only |
| SHARC | macro `DK_LOG level, code, value` in `dk_machine.inc`; expands to nothing when `DK_LOG_LEVEL` at assembly time is below `level` (release builds), else to: load the row's level word, compare, branch (about 3 instructions when the level is off), else store one record (`code | level << 16 | voice << 24`, `value`) into a per-row ring | no CALL, no allocation; the ring is owned by the adapter, in the module's reserved range, written only by the DSP; at most 4 records per voice-block, overflow counted; the oldest record is overwritten |
| declared codes | the manifest lists `[[log]] code = 3, name = "line_clamped", args = ["x"]`; the build generates the constants for the assembly and the table for the decoder | a code not declared fails the build |
| conformance | the rig runs with `DK_LOG_LEVEL = trace` and with it off: the output block must be bit-identical, and the cost at off is below 10 instructions per block |

How the records reach the ColdFire: the DSP-to-ColdFire channel today is the reply record, of which the diagnostics use rows 1 to 12 (`51:59-61`); its capacity for a log is **[O]**. Proposal: the reply
record carries the ring's write index and the newest two records per row each block (the channel that exists), and DSP peek (13.5) reads the rest when a gap is seen. The runtime turns records into ordinary events with the module's index
and gen, so one log, one decoder.

### 13.4 Module-scoped events

Load, DSP part done or failed, fault, disabled, re-enabled at next start, budget. These kinds exist (MOD_LOAD 5, MOD_DSP 6, MACHINE_FAULT 27, MACHINE_DISABLED 28) and gain the source bits; one new kind MOD_BUDGET
reports declared against observed cycles per module. The observed figure needs a cycle counter read around the call in the adapter (a SHARC clock register; **[O]**, to be read in the manual); without it the budget event carries
only the declared figure and the HEALTH block time.

### 13.5 Runtime services (framework, not module code)

| Service | What the SDK must provide | Phase 10 |
| --- | --- | --- |
| DSP peek, read-only | the module table gives `dsp: [lo, hi)` and the state range, so a dump can be named by module; the allowed DSP ranges are a device-profile list like `peek` | item 4 |
| DSP exception handler | writes the faulting PC and status into the reply record; the adapter and the table translate a PC into module + offset (needs the `dsp` range of every record) | item 4 |
| PC sampler | a timer ISR keeps a histogram ring of PCs; the table maps them to modules; gives per-module cost on the real chip | item 4 |
| named dumps | `[[dump]] name, space = "cf"|"dsp", addr, len` in the device profile, plus automatic names `module:<id>:state`, `module:<id>:image` from the table | item 6 |

### 13.6 Emulator parity

One transport (LOG, PEEK, LOGCFG over the same SysEx; the emulator answers as the device does), one decoder, and one source of kind and code names: `dklog.KINDS` is generated from `dkrt_sdk::logk` and the module
`[[log]]` tables, not copied by hand. Event time is the block counter on both, so event sequences align; wall time and cycle figures are not compared. A dump is taken at a marker (a log event such as MACHINE_CHANGE or a given
module event) on the device and in the emulator, and `dklog diff` reports the dumps and the event sequences that differ; a difference is a candidate emulator gap (finding 46 "Open"). Modules are deterministic under the
conformance rules, so a state dump of a module after N blocks is comparable. The contract requirement is that every module state and log record is reachable by name through the table, and that the emulator runs the same runtime and
adapter objects (it does, as the image is the same).

### 13.7 New open questions

8. Hot reload: when a swap ends sounding voices, is a click acceptable, or must the swap wait for the transport to stop?
9. Does the selection record have room for the log configuration, or does a second small record go in the store range?
10. Capacity of the DSP-to-ColdFire reply record for a log, and a cycle counter register for per-module cost; both need a read of the manual and a probe.
11. Is a row growth to 24 words now (for `gen`, level, state base, stride, clear bytes) acceptable? It costs one DSP section version and one runtime version, which section 6 needs anyway.
