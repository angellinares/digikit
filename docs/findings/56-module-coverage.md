# Module coverage map (DT2 1.17, DN2 1.12)

What the module system can do today, per module type and device, and what checks it. Read from the findings, the
tests, the scenario files and the smoke matrix on 2026-10-21 (main d6b520f0); nothing was run for this map. All rows
are **[D]** (read once); a second agent checks them against the code before any row is marked [V].

Codes per device cell:

| Code | Meaning |
| --- | --- |
| Gs | a smoke cell (tests/patchkit/smoke/matrix.toml): runs at every non-docs gate |
| Gr | a routed gate suite (machine conformance, patchkit, rust-tests): runs when the change triggers it |
| Gq | a digikit scenario at the quick tier (tests/digikit/scenarios/): runs when the digikit-boot suite triggers |
| Gf | a digikit scenario at the full tier: runs only in `verify --full`, which runs only after generated-core changes |
| P | a proof tool or one recorded emulator run, not gated |
| H | ran on a real device |
| S | static evidence only |
| U | unknown or never tried |
| N | not supported or not built |

## 1. Summary

- **[D]** One device, one module type on hardware: everything that ran on a device ran on the Digitakt II, and the
  only content module that ran there is the pluck machine (finding 47; plan device tests of 2026-10-20 and 2026-10-21).
  Nothing has run on a Digitone II.
- **[D]** Run-time machines work on both devices in the emulator and through one adapter (core/dsp-voice). Run-time
  effects do not exist: core/fx hosts only patched-in effects; run-time fx records are P3d-1b (not started).
- **[D]** Raw ColdFire code modules (asm, C, Rust) load at run time on both devices, but most of their scenarios are Gf,
  so the per-PR gate rarely runs them.
- **[D]** No smoke cell loads a run-time module; every smoke cell is patched in. DN2 has no core/fx smoke cell.
- **[D]** The loader has capacity limits and refusal codes; every one is filled and refused with its code by `capacity-*` (finding 57; **[C]** this row said no test filled a limit).

## 2. Module types x device: can a run-time .dkm of this type load?

| Type | DT2 1.17 | DN2 1.12 | Exercised by |
| --- | --- | --- | --- |
| machine with DSP | Gq, Gr, H | Gq, Gr | digikit/pluck: equiv-dyn-pluck, pages-dyn-pluck, menu-dkrun-pluck; machine conformance; device: finding 47 rung C4, rt020, rt021 |
| machine, data only | Gf | Gq | test/dyn-xslice (equiv-dyn-xslice, DT2); test/dyn-dn2 and test/sine (equiv-dn2-set, DN2) |
| fx (run-time) | N | N | P3d-1b; `[contract] fx` is [0, 0] |
| fx (patched in, through core/fx) | Gs, Gr, P | Gr, P | test/fx-gain: smoke cell DT2 only, packages/sharc-rig-kit/conformance/fx.test.ts, pk_proof_fx |
| raw ColdFire code (asm) | Gq, Gf | Gq, Gf | test/dyn-demo (memaudit-dt2/dn2, equiv-dyn-demo), test/dyn-fault, test/dyn-crash |
| raw ColdFire code (C) | Gq, Gf | Gq, Gf | test/c-demo (memaudit, equiv-c-demo) |
| raw ColdFire code (Rust) | Gf | Gf | upload-rust; test/rust-demo is patched in (smoke cell DT2 only) |
| service | N | N | manifest type exists; no module, no hook |
| UI page | N | N | SETTINGS > DIGIKIT is the patched-in core/digikit-menu; a UI hook does not exist |

## 3. Adapters (core/*, all patched in)

| core/ | Role | DT2 1.17 | DN2 1.12 |
| --- | --- | --- | --- |
| expansion | reset-path loader for patched-in code | Gs | Gs |
| machines | adds machine types | Gs | Gs |
| params | adds parameter rows | Gs | Gs |
| dsp-voice | voice adapter, contract path and raw path, guard, clamp | Gs, Gr, H | Gs, Gr |
| dsp-link | sends code and data to the running DSP | Gs, H | Gs |
| dynreg, dyndsp | reserve machine, parameter, page and DSP rows for run-time machines | Gq, H | Gq |
| runtime | loads .dkm files from the store at boot; USB commands | Gq, Gf, H | Gq, Gf |
| digikit-menu | SETTINGS > DIGIKIT > MODULES, ABOUT | Gs, H | Gs, Gf |
| health | CPU, DSP and heap figures | P, H (rung A) | U (no smoke cell) |
| fx | effects adapter, chain of up to 4 | Gs, Gr, P | Gr, P (no smoke cell; emulator cost x3.09, finding 55) |

## 4. Runtime features

| Feature | DT2 1.17 | DN2 1.12 | Notes |
| --- | --- | --- | --- |
| upload over USB (`dk:run`) | Gq, Gf, H | Gf | upload-quick is DT2 only (finding 43) |
| load from the store at boot | Gr, H | Gr | tests/patchkit/test_patchkit_dkrt.py |
| several modules at once | Gq, Gf | Gq | memaudit (3 modules), equiv-mixed (Gf), equiv-dn2-set (2); `capacity-*` (8 modules), `memaudit-*-full`, `isolation-*` (finding 57) |
| unload or replace while running | N | N | the module set is fixed per boot; replace a store file: upload-ops (Gf) |
| same-hash collision rule | U | U | implemented, no test (finding 52) |
| contract version refusal (E_CONTRACT) | Gr | Gr | test_patchkit_dkrt.py boot results |
| `runtime_min` check | N | N | not checked by the loader |
| DSP write-set check (P4a) | Gr | Gr | rig only (Python SHARC core), machines and effects |
| ColdFire memory audit (P4b) | Gq | Gq | memaudit-* and the -stray negative controls (the DN2 ones were not run by any pytest file until finding 57 added `test_debug_dn2.py` **[C]**); DMA, host writes and runtime-API stores are not seen |
| guard (non-finite or above 8.0; 8 faults switch a type off) | Gf, H (present, 0 faults; never fired on a device) | Gf | debug-guard |
| sticky machine ids | Gq, H (type 16 assigned on rt020 and rt021) | Gq | sticky-types; finding 52 |
| 16-voice polyphony | U | U | conformance uses 16 only to divide the budget |
| `max_cycles` budget | Gr (rig) | Gr (rig) | declared and checked in the rig; not enforced at run time |
| percent and bipolar parameters | Gr | Gr | conformance drives min, max, mid |
| enum parameters (`DK_P_ENUM`) | U | U | no module uses one |
| display templates other than 0xd1 / 0xd5 | U | U | |
| p-lock of a module parameter | Gq (stored, round trip) | Gf | roundtrip-dt2, roundtrip-dn2; the lock value reaching the DSP is not checked |
| LFO on a module parameter | U | U | the MOD page lists the names; DSP effect not known |
| LFO on TUNE | Gr, H | Gr | pluck range tests; device: crackle at large depth (pluck design, cookbook) |
| MIDI CC and NRPN on added rows | N | N | ignored by the firmware |
| parameter persistence (project save and load) | Gq | Gf | roundtrip scenarios |
| DT2 knob 3 refused on a SAMPLE clone | Gr | n/a | packages/patchkit typed.rs unit test |

## 5. Memory safety per module type

What stops a module from writing outside its own memory, and what checks it.

| Type | Placement | Protection at load | Checks of writes while running |
| --- | --- | --- | --- |
| machine (DSP) | DSP range from `alloc_range` (first fit, no overlap with other ranges), per-voice state inside the range, voice row from a free list (modules/core/runtime/src/rs/src/dsp.rs) | ranges and row owners cross-checked (modules/core/runtime/src/rs/src/table.rs) | P4a write-set check in the rig (out block, own state, stack, own data); the guard checks only the output; P4c canary on the device is planned |
| machine (ColdFire tables) | arena block (modules/core/runtime/src/rs/src/arena.rs: first fit, 16-byte aligned, coalescing) | arena bounds per module (table.rs) | P4b memory audit while the UI is driven |
| fx (run-time) | N | N | the FX rig has a write-set check (`FxRig.foreign_stores`), used for patched-in effects |
| raw ColdFire code | arena block | arena bounds, relocations and imports checked (modules/core/runtime/src/rs/src/loader/mod.rs) | P4b memory audit; not seen: DMA, host writes, runtime-API stores |

Gap closed **[C]** by finding 57: `memaudit-*-full` runs the audit with every limit used and three modules refused, and `isolation-*` checks that a reference sound keeps its PCM, ranges and DSP bytes behind the full set.
The write-set check (P4a) still looks at one module at a time.

## 6. Capacity per module type

| Resource | DT2 1.17 | DN2 1.12 | Defined in | Refusal when full | Tested |
| --- | --- | --- | --- | --- | --- |
| modules loaded | 8 | 8 | device profile `[limits] modules` (at most `[limits.max]` of sdk.toml = `SEL_MAX`) | SELECT of a ninth path `E_SEL_COUNT`; `E_TABLE_FULL` is unreachable while `modules` is `SEL_MAX` | capacity-* (Gq) |
| ColdFire arena | 512 KiB | 512 KiB | `[limits] arena_bytes` | `E_NOMEM`; a bss above the arena is `E_FORMAT` | capacity-*, memaudit-*-full |
| upload stage buffer | 64 KiB | 64 KiB | `[limits] stage_bytes` | `E_TOO_BIG` (a file of the exact size loads) | capacity-* |
| machine types and DSP voice rows | 8 (types 0x10-0x17) | 8 | `[limits] machine_rows`, tied to types.toml by a gen check | a hashed machine `E_TYPE_RANGE`; a pinned voice `E_DSP_ROWS` (rows) or `E_DSP_TYPE` (type) | capacity-*, isolation-* |
| parameter rows | 16 | 16 | `[limits] param_rows` (tied to the core/dynreg manifest by a cargo test) | `E_PARAM_FULL` | capacity-* |
| page rows | 0 | 8 | `[limits] page_rows` (tied to the core/dynreg manifest) | `E_PAGE_FULL` (DN2) | capacity-dn2 |
| sticky id map entries | 8 | 8 | `MAP_N` in the runtime SDK (`[limits.max] map_entries`, a cargo test ties them) | eviction (`MACHINE_TYPE_EVICTED`) | sticky-types |
| DSP run-time area | 512 KiB | 320 KiB | device profile `[digikit] dsp_area_bytes` | `E_DSP_NOSPACE`; granules of 64 bytes, the first range starts at offset 4, 60 bytes unusable | capacity-*, memaudit-*-full |
| effects in the chain | 4 | 4 | `[limits] fx_slots` (at most `DK_FX_MAX`) | patched in: build time (`apply` refuses more than `fx_slots`) | prims test |

The numbers `MAX_MODULES`, `ARENA_BYTES`, `SEL_MAX` and `FX_MAX` that tools/patchkit repeated are now read from the profile; a test fails when a literal returns.
Pluck takes 256 KiB of the DSP area, so on DN2 a second module with a delay line does not fit.

## 7. Memory use reporting

- The runtime fills `DkrtHealth` with totals, free bytes and largest free block for the arena, the DSP area, the heap
  and the store; `DkrtModule` gives each module's arena `size` and `dsp_size` (modules/core/runtime/sdk/src/lib.rs).
- The MODULES page prints the free DSP area; the health log record carries `arena_free` and `heap_free_kb`.
- Not reported: use per module type, rows used and free per type (machine rows, effect slots), DSP state bytes per
  module. **[C]** Rows used and free per type are reported by the CAPACITY command since runtime 0.2.2 (finding 57); DSP state bytes per module still are not.

## 8. Device log

| Date | Image | Module | Result |
| --- | --- | --- | --- |
| 2026-10-18 | rung A, B | none | boots, log over USB, DSP link runs, audio normal (finding 47) |
| 2026-10-18/19 | rung C to C4 | dyn-pluck | upload works; SHARC halt from 17 excluded stores, fixed; plays from C3d2 (finding 47) |
| 2026-10-20 | rt020 | test/dyn-pluck | INFO 0.2.0, MODULES type 16, 0 faults, DSP 42 %; old project showed a blank machine (led to type_hint) |
| 2026-10-21 | rt021 | digikit/pluck | plays, guard 0 faults, DSP 42-46 %; LFO on TUNE at large depth crackles (cookbook) |

Digitone II: no run on a device. Its long-word clear stores and its module store medium are unverified.

## 9. Gaps, largest first

1. Run-time effects: no fx record, no run-time effect on either device (P3d-1b).
2. ~~Capacity: no test fills a limit; no test checks that loaded modules stay intact when another is loaded or refused.~~ Closed by finding 57 **[C]**.
3. Most raw-code and safety scenarios (guard, safe mode, crash capture, uploads, Rust) are Gf and rarely run.
4. Digitone II: nothing on a device; no smoke cell for core/fx or health.
5. Parameter paths: enum, other display templates, LFO on a module parameter, the p-lock value at the DSP.
6. Polyphony at 16 voices and the run-time cycle budget are not measured.
7. Reporting per module type (rows, bytes) does not exist.

## 10. Stale statements corrected

- Section 6 said no test fills any limit: finding 57 fills them all. The DSP area's first range starts at offset 4 and 60 bytes stay unusable.
- The four `memaudit-*` scenarios were called quick, but the two for the Digitone II had no pytest file; `tests/digikit/test_debug_dn2.py` runs them now.
- Finding 51 said the guard has not run on a device: corrected there (rt021, 0 faults; it has not fired on a device).
- Finding 54 said the pluck device result was being re-tested: the result of 2026-10-21 is now in its entry.
- The plan's "Gaps to close before the public site" said the Digitone II has no runtime guard: P3b-1 added it
  (debug-guard runs on DN2).
