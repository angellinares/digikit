# SDK and runtime roadmap: contract, runtime, profiling

Date: 2026-11-10. Status: **draft for Em's review.**

This file holds the one order for the items of these plans. The plans hold the content and the dependencies of their items; they hold no
order of their own.

- **R** = [runtime-and-services.md](runtime-and-services.md), section 13 (items) and 13.1 (dependencies).
- **P** = [profiling.md](profiling.md), section 13 (items and dependencies).

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
| 1 | R1 | SDK cleanup with hash task | - | todo | |
| 1 | R4 | Spike, static part: can SHARC hooks move into a `.dks` | R1 | todo | |
| 1 | P1 | Address map v1, emulator only | - | todo | |
| 1 | P7 | Clock audit (static) | - | todo | |
| Stop | | Review the R4 answer with Em: it decides the owner of DSP dispatch and where `DK_SPAN` lives | R4 | | |
| 2 | P2 | Shared Rust library, `wb:cli` profile and addrmap commands, JSON output | P1 | todo | |
| 2 | P3 | sharcdb tables for module DSP sections | P1 | todo | |
| 2 | P4 | Per-module cost, machine budget check, fast-tier refusals per module | P2, P3 | todo | |
| 2 | R5 | SDK contract, with DSP rules, derived cycle cost and `DK_SPAN` shape; existing modules ported | R4, P3, P4 | todo | |
| 3 | R2 | Runtime deletions and split; sink slots | R5 | todo | |
| 3 | R3 | Platform adapter, layer rule | R2 | todo | |
| 3 | R6 | DSP transfer queue; dsp-link windows | R3 | todo | |
| 3 | R7 | Generic records and service registry; then the new-kind proof (an LFO shape, no runtime edits) | R6 | todo | |
| 3 | R8 | Spike, whole path | R7 | todo | |
| 3 | P6 | Workbench: names in the Log pane, first utilisation pane (UI lane) | P2 | todo | |
| Stop | | Review the model with Em; the contract is proven by the new kind | R7, R8 | | |
| 4 | P8 | `DK_SPAN`, sink slot, `profile.dks`, DSP stats window | R2, R6, R4 answer | todo | |
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
