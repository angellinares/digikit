# R12-8: SHARC stubs, DSP dispatch in services

Date: 2026-11-17. Targets: DT2 OS 1.17, DN2 OS 1.12. Part of R12 ([runtime-and-services.md](runtime-and-services.md) 13.2); order in
[sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). Basis: [finding 128](../findings/128-r4-hooks-in-services.md) (SHARC hook sites cannot move into a
`.dks`), [finding 31](../findings/31-dsp-link.md) (dsp-link), [digikit-hot-reload.md](digikit-hot-reload.md) (S5), [runtime-slimming.md](runtime-slimming.md).

Status: **designed, not built.** Branch `r12-8/slice`.

## 1. Goal and scope

The `machines` and `fx` services own the per-block DSP dispatch. The image keeps only thin stubs at the hook sites.

**Moves into the services (`machines.dks`, `fx.dks`):** the dispatch code of the voice and effect hooks: mask scan, contract adapter, output guard,
breadcrumbs, `_fx_exec`, fxsend.

**Stays in the image:**

- All hook sites and their stubs.
- All adapter data: voice and effect rows, diag words, save words, per-track state.
- The DN2 stereo and pan code (25 hooks of `dsp-voice`). It and the dispatch share data both ways.
- `_dv_exp2`, `_dv_frame_hw`, `_dv_frame_sw` and `_dk_step`. Module DSP code imports resolve only against the image.
- The voice-0 heartbeat words.

Only dispatch code moves because the stereo code and the dispatch use the same data in both directions, and module DSP imports resolve only
against the image.

Not in scope: `DK_SPAN` call sites (P8d), the DSP stack guard (later), any change to `sharc_core` or the transpiler (no `CORE_FINGERPRINT`
change), any PCM pin change.

## 2. Design

### 2.1 One stub per hook site

Each hook site that moves its dispatch gets one stub in the image. The stub:

1. Does the voice-0 heartbeat and context words that the displaced code did (the heartbeat stays in the stub).
2. Loads its slot once.
3. Slot 0: runs the displaced instruction, then stock code.
4. Slot not 0: saves and pins one M register, then jumps to the slot address.

Four of 16 slots are used: `voice.render`, `voice.unpack` (DT2; the two unpack sites share it), `fx.entry`, `fx.send`. A device with no service file
or a failed service has all slots 0 and runs stock.

### 2.2 The slot block

The slots are 16 words that dsp-link owns. They are zero at boot because the boot image initialises them. The host addresses them as words 0-15 of
dsp-link window 0, and the area allocator starts behind them.

The slots do not live in `_dl_area`. `_dl_area` is a `.PKBSS`: reserved, not part of the boot stream, and it holds garbage at power-on
([finding 31](../findings/31-dsp-link.md), the `_dl_area` paragraph). A slot read from there at boot would jump to garbage.

### 2.3 DSP section in the services

`machines.dks` and `fx.dks` gain a DSP section (`dks/dsp/*.s`) with today's dispatch code. Image data it uses (rows, diag, save words, `_dv_exp2`,
`_dk_step`) arrives as DSP imports, resolved against the image through the `dyndsp_info` table. The helper routines stay in the image. The services
`require core/dsp-link`.

### 2.4 Slot declaration and fill

- The adapter `[[slot]]` (DSP address space) gets an index in dsp-link's slot block and an address in the info table.
- A service `[[fill]]` becomes a SLOTS item: slot name and the entry symbol of the service's DSP section.
- Slot indices of one service are contiguous, so one link chunk writes them all.

### 2.5 Loader and installer

The core stays kind-free: it has a generic provider DSP load and a generic slot fill. It knows no voice, effect or machine.

The `Installer` becomes a trait with two backends (it was a concrete struct until R12-8 adds a second backend). The ColdFire backend is unchanged. The
DSP backend, after the provider's DSP code is acknowledged, sends one link chunk that writes the service's slots. Providers follow the module states
DSP loading, then active or failed.

There is no version word. The stub-contract `.inc` (register pinning, slot semantics, the displaced-instruction rule) joins the input of the SVC_ABI
hash. A service built against another contract is refused with E_SVC_ABI.

Boot order: the boot loads the ColdFire half of every provider, then the modules, as today. The provider's DSP job is queued first, before the jobs of its dependents. The ColdFire half is live from boot, the slots stay 0 (stock audio) until the provider's DSP code is acknowledged, and dependents show DSP loading until then. Settle runs at boot as before: the boot hook cannot wait for the DSP, because the frames that carry DSP code do not flow in it.

### 2.6 Failure rule

All or nothing, as on the ColdFire side. The DSP part belongs to the service. If it fails (transfer, relocation, import, fill), the whole service
fails:

- The ColdFire hooks go back to stock, the slots stay 0, and the service has no export, site or block.
- The service goes through the shared unload lifecycle, the one that unloads a module whose DSP part fails (clear slots and row table, drain counters, `fini`, registry remove, release DSP, free). Its dependents (by NEEDS) go through the same lifecycle with E_MISSING in their module states, and their queued DSP jobs are removed. The services get `Service::remove(owner)` calls as for any unload. The core has no kind-specific code in this path.
- Audio is stock.

This is late teardown: provider failure at the ColdFire stage is still refused at boot (`load_providers` and settle), and a failure of the DSP part takes the service down afterwards by the same lifecycle that modules have (a DSP transfer failure makes the whole module FAILED).

The zero-DSP test peer never acknowledges a DSP transfer (`docs/TESTING.md`): a service with a DSP part needs a coupled session, and the test harness refuses it in any other mode.

### 2.7 Hot reload (S5, later)

Load the new DSP section beside the old one, rewrite the slots in one chunk, wait two confirmed blocks, free the old section. The stub reads its slot
once per call, which makes this safe. Invalidating the DSP instruction cache before reuse stays open ([digikit-hot-reload.md](digikit-hot-reload.md)).
The DSP backend of the Installer trait is where it goes, and the unload lifecycle is the one hot reload uses for services and modules ([digikit-hot-reload.md](digikit-hot-reload.md), "Where the code lives"): the slot swap of S5 is the same slot write as the install, and a failed install is the same teardown as an unload.

## 3. DK_SPAN (R4 reconciliation)

R4 ([finding 128](../findings/128-r4-hooks-in-services.md)) put `DK_SPAN` and the stack guard in the stub. The stub cannot do this: it knows no row,
and a span needs the row as its owner.

- The mechanism (`_dk_stats_ptr`, `_dk_span_t0`, `_dk_span_add`) is image-owned.
- The call sites (`DK_SPAN_BEGIN/END`) belong in the service dispatch, around each row call. They come with P8d, not with R12-8.
- The stack guard word stays "later" (runtime-and-services section 8).

R12-8 is a pure move: PCM and `dsp_cost` stay flat.

## 4. Decisions

1. All-or-nothing DSP loading (2.6).
2. Pure move: no `DK_SPAN` call sites, no stack guard word.
3. The visible DSP area gets smaller. Firmware DSP addresses come from the image import table. `dsp-voice` and `fx` `requires core/dsp-link`.

## 5. Steps

Each step runs its own host tests. Device runs follow the batch rules in AGENTS.md.

| Step | Content | Tests |
| --- | --- | --- |
| 1 Rig first | Split `dsp-voice.s` into stub and dispatch. The rig kit (`place.ts`, `voice.ts`) links both and pokes the slot. | Rig cases: zero slot gives stock, a poisoned bss gives no jump, a filled slot dispatches |
| 2 dsp-link slot block | Window-0 mapping of words 0-15, `dyndsp_info` entries, allocator start behind the block | `dkrt-fit` and link host tests |
| 3 Patchkit and format | DSP `[[slot]]` and `[[fill]]`, the `.dks` DSP section (`dks/dsp/*.s`), `service-check`, stub contract in the SVC_ABI hash | Packed `svc-test` fixture with a DSP section; service-check cases |
| 4 Loader and installer | Provider DSP load, `Installer` trait with the DSP backend, SLOTS fill, failure rule | `Loader` and `FakePlatform` order tests; assembly case; refusal E_MISSING of a dependent |
| 5 Slice gate (own go point) | DT2 voice render through the stub and `machines.dks` | gate-pins at a zero slot; the `test/sine` PCM pin; `sharc_cells` instruction ratio. Changes image hashes, `.dko` objects and smoke budgets |
| 6 Remaining sites | DN2 render, DT2 unpack, fx entry and send | The machine fixture suite and the FX contract suite |
| 7 ColdFire leftovers | Items parked "with R12-8" in [runtime-slimming.md](runtime-slimming.md): `guard_*`, the voice and fx fields of `DspState`, `dsp_info`, `fx_info` into the services, the addrmap reader | `FakeCore` tests |
| 8 Docs | runtime-and-services, roadmap, AGENTS.md; `smoke --update` for touched cells | |

Before step 4: count the DSP imports against `MAX_IMPORTS` (fallback: one data-block import), and list the zero-DSP tests that load `machines.dks`.

## 6. Tests

- The dispatch is proven on the rig (step 1), not by per-module device tests.
- Device tests at real boundaries only: the machine fixture suite, the FX contract suite, one case with no service file (stock), gate-smoke and
  gate-pins.
- Expected: no PCM pin change; golden SHARC hashes change only for images with stubs (explain in the PR).
- Performance: the ceiling first (stub cost per call times calls per block on the 32-voice path), then the slice's `sharc_cells` ratio, then one
  paired `coupled_ab` run for area-code speed in the emulator tier.

## 7. Risks

- Stub overhead on the 32-voice path. The slice measures it.
- Area code may run slower than image code in the emulator tier. One paired A/B decides.
- DSP import count against `MAX_IMPORTS`. Count first.
- Zero-DSP tests that load `machines.dks` need DSP acknowledgement (2.6).
- `dsp_cost` may shift if the rig's cost includes the stub.
- Conflict with branch `test/t2-machine-rig-refs` (rig pins and `place.ts`/`voice.ts`).

## 8. Cost and order

Payoff: the image holds no machine or effect dispatch, and hot reload S5 and R14 (adapter cost) work on service code. Confidence: medium; the
slice decides.

Estimate: about 16-22 agent hours; the slice (steps 1-5) 3-4 hours. Device wall time: about 25 minutes for the slice gate, 60-90 minutes for the final
subset (DT2 and DN2).

Order: starts on top of B1 and RS-C. It merges after the shared B1, RS-S and RS-C gate.
