# T2 step 2: machine PCM pins to rig references

Replaces the PCM pins of `modules/machines/{analog,bd,pd,pluck}/tests/equiv.device.test.ts` with sound-check pins on the SHARC-only rig
([plan](../test-architecture.md) "T2"). The dynreg pin (`modules/core/adapters/dynreg/tests/equiv.device.test.ts`, DT2 sine and DN2 set) stays.
The deletion is the last commit of the branch. Do not merge it before the R12-6 machines.dks gate and Em's review of the new pins.

## The new pins (for review)

File per module: `modules/machines/<m>/tests/rig/equiv-note.toml`, run by `pnpm nx:pilot run <module id>:test-rig` (host only, no emulator).

| What | Device pin (old) | Rig pin (new) |
| --- | --- | --- |
| Note | 60, velocity 100, TRIG 1 held | 60, velocity 100, one trigger |
| Parameters | the menu defaults | the manifest defaults (rig `MachineRig` writes `p.default` of each parameter) |
| Notes | one | pluck: a second note 67 after the first (750 blocks) |
| Length | 24000 interleaved float32 samples (0.25 s) from the onset | 375 blocks of 32 samples (0.25 s), mono machine output, from the trigger |
| Signal | the whole firmware path: machine, track filter, mixer, output | the machine's own DSP through the dsp-voice adapter; no firmware |
| Voice state at start | uninitialised L2 for analog and pd (`UNINIT`) | every case runs twice with all module state filled differently (NaN words, then a seeded integer pattern; the module's bss and the voice state blocks) and fails when the outputs differ: the output does not depend on the initial state. The adapter's `note_on` clears the first `clear` bytes of each voice state, as on the device. A machine that reads unwritten state is a machine bug to report, not a pin |
| Pin | SHA-256 of the PCM window | SHA-256 of the float32 output words (`same-as`), one case per device |
| Also checked | audible | `peak >= 0.01`, `no-clip`, and the implicit rules of every rig case (finite output, context registers, own memory only) |

How the values were produced: `DIGI_RIG_PIN=1 pnpm exec vitest run --config vitest.host.config.ts --project module-rig modules/machines/<m>/tests/`
runs each case and writes the hash into the `sha256` line (`pinRigFile` in `packages/sharc-rig-kit/src/soundcheck.ts`). Re-running without `DIGI_RIG_PIN` checks them.
The pins are new values, not copies of the device hashes: the signal differs, so they cannot be compared with the old SHA-256 values. What they pin is that
the machine's DSP output does not change; whether the firmware path adds anything is covered by the dynreg pin and the device conformance load.

Pluck also keeps its model tables (`tests/rig/pluck.toml`); these are the older pins of the DSP against the float32 model, on DT2.

## Pinned cases (written 2026-11-16 by `DIGI_RIG_PIN=1`, poisoned memory, friendly context, velocity 100, manifest-default parameters)

| Module | Device | Note(s) | Blocks | sha256 | Peak |
| --- | --- | --- | --- | --- | --- |
| fadedlimes/analog | dt2 | 60 | 375 (0.25 s) | `e19439ebb963cb83cba8648741c24c013c95d3f8ca65b88393a9c4dbccc5ef4f` | 0.1938 |
| fadedlimes/analog | dn2 | 60 | 375 (0.25 s) | `f2c3d90f3742fa9a6045836375885b9c2f8c17edfa60ac00bbab2ecd7e6abeaf` | 0.1938 |
| fadedlimes/bd | dt2 | 60 | 375 (0.25 s) | `cb132603f4a8e9ab9c89b05b48a451f9f1a5ee664724c2a9246e8f2d4a3675e8` | 0.2756 |
| fadedlimes/bd | dn2 | 60 | 375 (0.25 s) | `53b22b3a46fc2cef2588813985d6f69dff64410dc60f9141f019b3d104f79e39` | 0.2756 |
| fadedlimes/pd | dt2 | 60 | 375 (0.25 s) | `61967e133943c4c20194877bfeec5b561b2edd0f2fb8bed908043c6520361118` | 0.1968 |
| fadedlimes/pd | dn2 | 60 | 375 (0.25 s) | `d8d7e2bf3aaa872a52b31ef623d1f3b74475a99a2e4033c2a10d5cccef4928a4` | 0.1968 |
| digikit/pluck | dt2 | 60, then 67 | 375 + 375 (0.5 s) | `65f2c9d932449430d967f2a748b888b5217c862d882db49b69ffbad1f85cfff8` | 0.8647 |
| digikit/pluck | dn2 | 60, then 67 | 375 + 375 (0.5 s) | `7cb2a7f0b53d52be338b7823c0d2d3f15020227aefe5b2621f7e23e7d0e18a17` | 0.8647 |

Peak: largest absolute output sample of the pinned signal (an independent rig run; the check requires at least 0.01, so every machine is audible). The peaks are the same on both devices while the hashes differ, to be looked at in review: the two devices differ in the trigger path, so a one-block shift would give this.

What the old device pin covered that these cases do not: the track filter (analog, pd and bd run through the track's stock filter), the mixer and the output path (levels, pan, master), the stereo interleave of the PCM, the exact onset and trigger timing of the firmware, the real card load and boot of the module, and the real voice state at boot (`UNINIT` on analog and pd). They cover what the old pin could not tell apart from firmware effects: the machine's own DSP output alone, with poisoned state.

DT2 against DN2 (host rig, both outputs dumped, 2026-11-16): the hashes differ because DN2's note starts one block (32 samples) later than DT2's. The first differing sample is 32 for analog, 1 for bd and 0 for pluck; for analog, pd and pluck (one note) the DN2 output shifted by exactly 32 samples equals the DT2 output (maximum difference 0), and no other shift in -64..64 matches. Cause (the rig's model of the trigger, `trigger` at `machine.ts` and the DT2 and DN2 trigger paths in `block`; not measured on the device): the DN2 trigger is the rising edge of a frame bit that the voice sees one block after the call that sets it, while the DT2 trigger writes the voice record and sounds in the same block (`trigger` and the device comment in `packages/sharc-rig-kit/src/machine.ts`); the peaks are equal because the signal is the same. The pluck two-note case does not match as a whole only because the second note also shifts by a block while the first note's tail overlaps it. These cases stay per device: the shift is the real trigger contract of the two devices, so the cases pin each path. **bd is different and is a defect of the committed gallery, not a rig result:** `modules/machines/bd/build/gen/gallery.json` has a `dn2` entry only, so the DT2 spec of bd has no parameters and the rig writes no defaults (bend, index, ratio and noise stay 0), while DN2 writes 16384, 8192, 0 and 0. The two bd outputs therefore differ beyond a shift (maximum difference 0.55 after a 32-sample shift, with any parameter subset). The bd DT2 pin is therefore not a manifest-default note and must be re-pinned after `build/gen/gallery.json` of bd gets its `dt2` machine and parameters (analog, pd and pluck have both). See "Pending".

Not pinned: no machine produced NaN or failed the finite rule under poison.

## Assertion ledger

Each deleted test has the same assertions (`defineEquiv` in `tests/regression/runtime/equiv.ts`), except where noted.

| # | Deleted assertion (all four machines, DT2 and DN2) | New home |
| --- | --- | --- |
| 1 | The selection loaded at boot: module count equals the selection | dynreg equiv (fixtures, same boot path). Real module: `<id>:test-conformance` (`tests/conformance/assert-load.ts`) uploads the real `.dkm` over USB, reboots, and asserts that exactly the debug module and this module are registered, the bytes survive the reboot and the loader reports its version and needed runtime version |
| 2 | Every module load result is 0 | dynreg equiv (fixtures). Real module: the same suite asserts that the DSP loader completes the selected module with result 0 (MOD_DSP log), that the module has `faults = 0` and `guard_off = false`, and that a DSP part was allocated (`dsp_len > 0`) |
| 3 | Loaded count equals the selection and safe mode is 0 | dynreg equiv (fixtures). Real module: the same suite asserts that the boot log holds no SAFE, DSP_STOPPED, CRASH_FOUND, DSP_ABSENT, MACHINE_FAULT, MACHINE_DISABLED or TABLE_BAD record and that the UI is reached |
| 4 | One voice row is claimed by the machine | Weaker: the conformance load asserts `rows > 0` and exactly one published firmware type for a machine, not that one voice row is claimed. `test-conformance-rig` checks the gallery, not the runtime rows. Exact voice-row count: dynreg equiv only, for its fixtures |
| 5 | Pluck only: the address map resolves `_pk_render` to the module's DSP base (`dspName`) | machine fixture suite (R12-6), generic addrmap check: a fixture module with a DSP section (e.g. `test/sine`) resolves a module DSP symbol through the device load table. Another lane writes it; not part of this branch. Must be green before the deletion |
| 6 | PCM from the onset (24000 samples) has the pinned SHA-256 | `tests/rig/equiv-note.toml` (`same-as`), one case per device, see above. The signal differs (machine output, not the firmware path) |
| 7 | The note is audible (`peak > 0.01`) where no pin exists | `peak min 0.01` in the same cases |
| 8 | The rows (machine, parameter, page tables) equal a boot of no module, apart from the module's own machine | dynreg equiv for its fixtures (`test/sine`, `test/dyn-dn2`) only. The conformance load does not compare tables with a no-module boot. After the deletion nothing compares the tables of the real analog, bd, pd and pluck: see "No longer tested anywhere".
| 9 | No emulator fault during the note (`expectNoFault`) | Not tested for a note on the device. The conformance load calls `expectNoFault` after load and boot, without playing a note (it plays none and checks no audio: its own comment says DSP registration does not establish rendered audio correctness). The rig's implicit rules cover the DSP routine (finite output, context registers, own memory only) |
| 10 | Pluck, full tier DT2 only (`equiv-mixed`, note at frame 9500): audible | the second note (67) of pluck's rig pin case, which covers state after running a while |

Analog and pd's `UNINIT` boot (device-only) maps to the rig case that runs with two different state fills and requires identical output: the output does not depend on the initial state. Rows 1 to 4 and 8 are the part of the deletion for the reviewer to judge (row 5 needs the R12-6 fixture check).

## No longer tested anywhere after the deletion

Em decides whether each is acceptable. Nearest coverage in brackets.

- Track filter output of the four machines (dynreg DN2 pin: the stock path with a fixture machine, not these machines).
- Mixer, pan, master and the stereo interleave of the output (dynreg DN2 and DT2 pins and fixture suites, with fixture signals).
- Exact firmware onset timing of a trigger (dynreg pin onset; the rig models the trigger, see above).
- Firmware voice state at boot, `UNINIT` (now: the rig requires output independent of the initial state, so a machine cannot depend on it).
- Parameter and page tables of the real machines against a no-module boot (dynreg equiv, fixtures only).
- A real note of the real machine on the device: audibility, no fault while it plays (the conformance load plays no note; pluck's former full-tier audible case goes too).

## Deletion commits

Two commits, last on the branch, each marked do not merge before the R12-6 gate and Em's pin review:

1. analog, pd and pluck: the `equiv.device.test.ts` files and their `runtimeAcceptance` records. The pluck deletion needs the R12-6 generic addrmap check (row 5) to exist first.
2. bd: its `equiv.device.test.ts` and `runtimeAcceptance`, only together with the bd DT2 re-pin after `mise run module:check fadedlimes/bd` regenerates the gallery (see Pending).

After them, `test:subset -- S:machines` selects the remaining machine device tests (shot, tb303); analog, bd, pd and pluck are covered by `<id>:test-rig`.

## Pending

**bd DT2 pin: pending, gallery.json lacks dt2; re-pin after module:check regenerates it.** The bd DT2 case fails loudly until then (`galleryGaps` in `packages/sharc-rig-kit/src/spec.ts`, called by every machine sound-check case, names the missing device and the command). The hash in the pinned-case table for bd DT2 is not a pin. A host test (`tests/spec.test.ts`, "galleryGaps") covers the check with a fixture that lacks one device.

Until the gallery is regenerated, `@digi/sharc-rig-kit:test-conformance-rig` is red too: `MachineRig` refuses to open bd for DT2 (`galleryGaps`), which stops the machine conformance file. Every other machine opens.
