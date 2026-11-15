# SDK and runtime roadmap: contract, runtime, profiling

Date: 2026-11-10. Status: **draft for Em's review.**

This file holds the one order for the items of these plans. The plans hold the content and the dependencies of their items; they hold no
order of their own.

- **R** = [runtime-and-services.md](runtime-and-services.md), section 13 (items) and 13.1 (dependencies).
- **P** = [profiling.md](profiling.md), section 13 (items and dependencies).
- **S** = [sdk.md](sdk.md), section 12 (steps). S replaces the SDK parts of R1 and R5.

Goal: a stable SDK contract for modules (machines, FX, LFOs, filters, tools) first, then the runtime refactored under it, then profiling on
the device. Emulator profiling comes early, because the contract work needs it.

## Rules

- Start a session from this file, then the newest `docs/handovers/HANDOVER-*.md`.
- When an item lands, set its status to `done` with the commit or PR. When it starts, set `active` with the branch or lane.
- R items run one after the other, so an R item's "Depends on" can be the previous R item by order, not by content (content dependencies
  are in runtime-and-services 13.1). P items may run in their own lanes next to an R item.
- Before each item over a day: state payoff and confidence, and stop when they are weak.
- A change to the order is made here, with the reason in one line under "Changes".

## Order

Status: `todo`, `active`, `done`, `parked`.

| Phase | Item | What | Depends on | Status | Branch / PR |
| --- | --- | --- | --- | --- | --- |
| 1 | S1 | SDK restructure, byte-identical, with hash task | - | done | PR 416 |
| 1 | S2a | SDK C hygiene: clang-format, `-std=c11`, helpers into `core/`, SDK sources linked automatically | S1 | done | PR 424 |
| 1 | S2b | SDK entry point (`dk_module`, `dk_module_ctx`) and named imports by service; tools and fixtures migrated; the Api table, `dkm_init` and the slots deleted | S2a | done | PRs 426, 433 |
| 1 | S3 | SDK builders in `init` and the runtime commit path; machines, FX and fixtures migrated | S2b | done | PR 443 |
| 1 | S4 | SDK templates and docs | S3 | done | PRs 443, 446 |
| 1 | S3b | Delete the patched-in content path: the twins test/patched-pluck and test/twin-dn2, patchkit desc.rs and the typed lowering for patched-in machines, the macros in sdk/patched-in/include; move or drop the equivalence tests, smoke-matrix rows and the sharc_matrix input that use the twins | S3 | done | PR 449 |
| 1 | P7 | Clock audit (static) | - | done | finding 125 |
| 1 | P1 | Address map v1, emulator only (own lane) | - | done | PR 444 |
| 2 | R4 | Spike, static part: can SHARC hooks move into a `.dks` | S4 | done | finding 128 |
| Stop | | Review the R4 answer with Em: it decides the owner of DSP dispatch and where `DK_SPAN` lives | R4 | reviewed | DSP dispatch is owned by the `machines` and `fx` services; `DK_SPAN` lives in the image stub |
| 2 | P2 | Shared Rust library, `wb:cli` profile and addrmap commands, JSON output | P1 | done | PR 457; phase A (host): dkprof, dkprof.wasm, `wb` addrmap, module and profile commands, patchkit on the dksharc readers; phase B (one real emulator profile; `cfprof.py` removed after equal numbers) |
| 2 | P3 | sharcdb tables for module DSP sections | P1 | done | PR 450 (dksharc, Rust static analysis of module DSP sections, slice 1); this PR (P3b, DSP names in the address map: pack writes sharc entries, dkaddrmap decodes the load table from the DKRT_STATE/DKRT_DSP/dyndsp_info snapshot, readers are thin callers) |
| 2 | P4 | Per-module cost, machine budget check (fast-tier refusals moved to P5) | P2, P3 | done | done (this PR): `sharc.profile`, DSP attribution by function and loop in `wb profile report`, `patchkit budget` (Rust, `dkprof::budget`) behind `module:check` |
| 2 | S5 | SDK DSP contract: DSP rules, derived cycle cost, `DK_SPAN` | R4, P3, P4 | done | S5a PR 482 (derived DSP cost, shared selection check, DK_SPAN). S5b this PR: register def/use in dksharc, finding 130, register contract table in sdk.toml (clobber, keep, stack, never_write), rules check warns only, 0 warnings on real modules |
| 2 | S6 | SDK sample kernel: SIMD L/R, hardware loop, one word load per two frames, loop split at the wrap point. Shot costs about 5,050 (DT2) / 5,260 (DN2) instructions per 32-frame block; ceiling about 500-600 for the kernel; shot uses 91% of DN2 DSP headroom at 16 voices | S5 | todo | |
| 3 | R2 | Runtime deletions and split; sink slots; service anatomy: a service is a module with a role (settled before R7) | S4 | R2a, R2b, R2c, R2d done; R2 done after the gate | |
| 3 | R3 | R3a Platform trait; R3b adapter move (see runtime-and-services, Platform adapter) | R2 | R3a and R3b done (this PR); the image changes (two device words, the `core/platform` adapter object), so they gate together | |
| 3 | T1 | [Test architecture](test-architecture.md): tests follow layers, fakes and contract suites, service-scoped selection | T1a after R2c; T1c with R3 | T1a done (PR 471 baseline, PR 481 over-selection fixes); T1h done; T1c next | |
| 3 | T1c | [Host contract tests](test-architecture.md): first cases the runtime descriptor loaders on SDK-built fixtures (S5a's device gate found a drifted FX_DESC_BYTES) and one shared Storage contract suite (FakeStorage on the host, the real adapter once on the device) | R3 | todo | |
| 3 | R6 | DSP transfer queue; dsp-link windows | R3 | R6a done (this PR), R6b planned (trigger: modulation rate) | |
| 3 | R7 | Generic records and service registry; then the new-kind proof (an LFO shape, no runtime edits) | R6 | done (this PR) | one generated kind and service table, `dk_record_add`, the header table, the boot planner and its verdict, declare-check-commit, the `lfo` service ([runtime-and-services.md](runtime-and-services.md) 4.1) |
| 3 | R13 | Storage interface and files memory (split out of R7) | R7 | done | this PR: Storage trait in the Platform composition, store and DT2 file-system routes, listing memory from the arena (at most 2 listings; abandoned listings released after 10 s with E_LOST), .data -28,864 B, arena alloc/free under the irq mask (fixes a race on main) |
| 3 | R8 | Spike, whole path | R7 | done (spike, unmerged) | `rt/r8-spike` in lane sdk-ui, finding 131: a selected module loads a `.dks` provider from the store at boot (C and Rust), links its export, installs a ColdFire hook at a stock site, safe mode leaves it stock; both devices on the emulator; 21 of 54 ColdFire hooks movable now, 9 undecided (other-task reach), 13 too early (boot thread), 8 interrupt, 3 special |
| 3 | P6 | Workbench: names in the Log pane, first utilisation view (UI lane) | P2 | done (this PR): Log names, Memory per-module rows; Profile pane moves to P8 | ui/p6-names |
| Stop | | Review the model with Em; the contract is proven by the new kind | R7, R8 | reviewed | R9-R12 proceed as planned; R12 moves movable hooks into `.dks` and keeps boot-thread and interrupt hooks as image stubs with pointer slots (the finding 128 pattern) |
| 4 | P8 | `DK_SPAN`, sink slot, the debug module, DSP stats window, and the Profile pane (moved from P6) | R2, R6, R4 answer | P8a `prof/p8a-spans` complete in lane sdk-design (spans at module ticks, phases, polls; debug module sink with PROFILE_STATE/PROFILE_READ 0x1e/0x1f; runtime 0.8.1; DTIM0 clock read only; A/B B/A 1.001 with PCM equal); merge waits for the lfo flake fix. Profile pane next (own package; mockup before build) | |
| 4 | P9 | Device calibration; the broker's cycle budget uses the measured factor | P8; device transport proven on hardware, with Em's go | todo | |
| 5 | R9 | `fx` linked in | R8 | todo | |
| 5 | R10 | `machines` linked in; layer check final | R9 | todo | |
| 5 | R11 | Boot plan, module states, refusal fixtures | R10 | todo | |
| 5 | R12 | Services as `.dks`. Scope from R8: movable hooks in `.dks`; boot-thread and interrupt hooks keep image stubs plus pointer slots. Decisions carried from the R8 spike: table version bump; replace the `core/` prefix rule for runtime services; separate provider slots; patchkit wasm size (bind raised it past 2.5 MiB); uninstall and hot reload; a dedicated stock-mismatch error code | R11 | todo | |
| 5 | P10 | ColdFire PC sampling on the device | P9 | todo | |
| Any | T-AB | A/B of two firmware images: `coupled_ab --image LABEL=SYX` | - | done | PR 484 |
| Any | P5 | Track A: host profiles named from sharcdb (emulator host speed), in a lane | - | parked | |

Until P9, cycle budgets use emulator instruction counts with a margin. P9 replaces the margin with a measured factor; the contract field
does not change.

Later, only when a feature needs it: the firmware tooling list in [profiling.md](profiling.md) section 12.

## Changes

- 2026-11-10: first version. SDK contract (R5) moved before R2 and R3 (runtime-and-services 13.1). Track A parked: the focus is the SDK.
- 2026-11-12: the SDK design ([sdk.md](sdk.md)) replaces the SDK parts of R1 and R5 with S1-S5. S1-S4 come first; the R4 spike moves
  after them, because only `DK_SPAN` (S5) depends on its answer.
- 2026-11-12: S2 split into S2a (C hygiene) and S2b (entry point and imports), so the whitespace-only format commit and the binding change
  are reviewed apart.
- P3 was done in Rust (dksharc) instead of the Python sharcdb builder, because the workbench uses it through wasm. The sharcdb module mode was an oracle only and was not merged. Register def/use is dksharc slice 2 (done, S5b), needed by the S5b rules check.
- R2b was split into moves only (proven identical with `patchkit objdiff`); the interfaces and the body splits (registry `check`/`apply`/`remove`, dsp `check`/`reserve`/`step`) are in R2c.
- T1 (test architecture) was added: layers and fakes exist now, so test selection can follow layers ([test-architecture.md](test-architecture.md)).
- 2026-11-15: S6 SDK sample kernel added: the sample kernel is the largest DSP cost; it waits for S5 SDK DSP contract.
- 2026-11-15: T1c Host contract tests gets its own row: the runtime descriptor-loader case comes after a drifted constant (FX_DESC_BYTES); the Storage contract suite is deferred from R13 storage interface.
- 2026-11-15: P8 uses the debug module instead of `profile.dks`.
- 2026-11-15: R8 whole-path spike is done as an unmerged spike; R12 services as `.dks` is scoped to movable hooks.
- 2026-11-15: the lfo device test flake on main is an emulator model bug (USB MIDI delivered before the firmware stored its callbacks). It blocks the merges of S5b, R13 storage interface and P8a until fixed.
