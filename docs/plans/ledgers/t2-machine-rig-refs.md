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
| Voice state at start | uninitialised L2 for analog and pd (`UNINIT`) | poisoned: the module's bss and the voice state blocks are NaN words in every machine rig case (default `poison`, opt-out needs a `poison_reason`); a machine that reads unwritten state gives NaN and fails the finite rule, which is a machine bug to report, not a pin |
| Pin | SHA-256 of the PCM window | SHA-256 of the float32 output words (`same-as`), one case per device |
| Also checked | audible | `peak >= 0.01`, `no-clip`, and the implicit rules of every rig case (finite output, context registers, own memory only) |

How the values were produced: `DIGI_RIG_PIN=1 pnpm exec vitest run --config vitest.host.config.ts --project module-rig modules/machines/<m>/tests/`
runs each case and writes the hash into the `sha256` line (`pinRigFile` in `packages/sharc-rig-kit/src/soundcheck.ts`). Re-running without `DIGI_RIG_PIN` checks them.
The pins are new values, not copies of the device hashes: the signal differs, so they cannot be compared with the old SHA-256 values. What they pin is that
the machine's DSP output does not change; whether the firmware path adds anything is covered by the dynreg pin and the device conformance load.

Pluck also keeps its model tables (`tests/rig/pluck.toml`); these are the older pins of the DSP against the float32 model, on DT2.

## Assertion ledger

Each deleted test has the same assertions (`defineEquiv` in `tests/regression/runtime/equiv.ts`), except where noted.

| # | Deleted assertion (all four machines, DT2 and DN2) | New home |
| --- | --- | --- |
| 1 | The selection loaded at boot: module count equals the selection | dynreg equiv (same boot path, `test/sine`, `test/dyn-dn2`); device conformance load of the module |
| 2 | Every module load result is 0 | dynreg equiv; device conformance load |
| 3 | Loaded count equals the selection and safe mode is 0 | dynreg equiv; device conformance load |
| 4 | One voice row is claimed by the machine | `test-conformance-rig` (descriptor and voice claim through the gallery); dynreg equiv for the runtime row path. Dropped for the machine itself as a device assertion: the count is a descriptor property |
| 5 | Pluck only: the address map resolves `_pk_render` to the module's DSP base (`dspName`) | machine fixture suite (R12-6), generic addrmap check: a fixture module with a DSP section (e.g. `test/sine`) resolves a module DSP symbol through the device load table. Another lane writes it; not part of this branch. Must be green before the deletion |
| 6 | PCM from the onset (24000 samples) has the pinned SHA-256 | `tests/rig/equiv-note.toml` (`same-as`), one case per device, see above. The signal differs (machine output, not the firmware path) |
| 7 | The note is audible (`peak > 0.01`) where no pin exists | `peak min 0.01` in the same cases |
| 8 | The rows (machine, parameter, page tables) equal a boot of no module, apart from the module's own machine | dynreg equiv (same table diff); descriptor of the machine row from `test-conformance-rig` |
| 9 | No emulator fault during the note (`expectNoFault`) | implicit rules of every rig case |
| 10 | Pluck, full tier DT2 only (`equiv-mixed`, note at frame 9500): audible | the second note (67) of pluck's rig pin case, which covers state after running a while |

Analog and pd's `UNINIT` boot (device-only) maps to the poisoned rig case. Rows 1 to 4 and 8 depend on the dynreg pin and the device conformance load staying green (row 5: the R12-6 fixture check); they are the part of the deletion for the reviewer to judge.

## Deletion commit

Removes the four `equiv.device.test.ts` files and their `runtimeAcceptance` records in `project.json`. After it, `test:subset -- S:machines` selects the remaining machine
device tests (shot, tb303); analog, bd, pd and pluck are covered by `<id>:test-rig`.
