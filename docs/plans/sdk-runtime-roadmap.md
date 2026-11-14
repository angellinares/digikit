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
| 2 | S5 | SDK DSP contract: DSP rules, derived cycle cost, `DK_SPAN` | R4, P3, P4 | S5a done (this PR), S5b in progress | S5a: derived cost (`DSP_COST_VOICE`, `DSP_COST_FX`), shared `check_selection`, headroom and voices in the device profile, `max_cycles_*` deleted, `DK_SPAN` defined. S5b: DSP rules (dksharc slice 2, stock save-set finding) |
| 3 | R2 | Runtime deletions and split; sink slots; service anatomy: a service is a module with a role (settled before R7) | S4 | R2a, R2b, R2c, R2d done; R2 done after the gate | |
| 3 | R3 | R3a Platform trait; R3b adapter move (see runtime-and-services, Platform adapter) | R2 | R3a and R3b done (this PR); the image changes (two device words, the `core/platform` adapter object), so they gate together | |
| 3 | T1 | [Test architecture](test-architecture.md): tests follow layers, fakes and contract suites, service-scoped selection | T1a after R2c; T1c with R3 | todo | |
| 3 | R6 | DSP transfer queue; dsp-link windows | R3 | R6a done (this PR), R6b planned (trigger: modulation rate) | |
| 3 | R7 | Generic records and service registry; then the new-kind proof (an LFO shape, no runtime edits) | R6 | done (this PR) | one generated kind and service table, `dk_record_add`, the header table, the boot planner and its verdict, declare-check-commit, the `lfo` service ([runtime-and-services.md](runtime-and-services.md) 4.1) |
| 3 | R13 | Storage interface and files memory (split out of R7) | R7 | todo | runtime-and-services item 13 |
| 3 | R8 | Spike, whole path | R7 | todo | |
| 3 | P6 | Workbench: names in the Log pane, first utilisation view (UI lane) | P2 | done (this PR): Log names, Memory per-module rows; Profile pane moves to P8 | ui/p6-names |
| Stop | | Review the model with Em; the contract is proven by the new kind | R7, R8 | | |
| 4 | P8 | `DK_SPAN`, sink slot, `profile.dks`, DSP stats window, and the Profile pane (moved from P6) | R2, R6, R4 answer | todo | |
| 4 | P9 | Device calibration; the broker's cycle budget uses the measured factor | P8; device transport proven on hardware, with Em's go | todo | |
| 5 | R9 | `fx` linked in | R8 | todo | |
| 5 | R10 | `machines` linked in; layer check final | R9 | todo | |
| 5 | R11 | Boot plan, module states, refusal fixtures | R10 | todo | |
| 5 | R12 | Services as `.dks` | R11 | todo | |
| 5 | P10 | ColdFire PC sampling on the device | P9 | todo | |
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
- P3 was done in Rust (dksharc) instead of the Python sharcdb builder, because the workbench uses it through wasm. The sharcdb module mode was an oracle only and was not merged. Register def/use is dksharc slice 2, needed by S5.
- R2b was split into moves only (proven identical with `patchkit objdiff`); the interfaces and the body splits (registry `check`/`apply`/`remove`, dsp `check`/`reserve`/`step`) are in R2c.
- T1 (test architecture) was added: layers and fakes exist now, so test selection can follow layers ([test-architecture.md](test-architecture.md)).
