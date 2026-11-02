# Python test triage (plan, 2026-11-02)

Status: triage done (read-only scouts, 2026-11-02); duplicate checks done; small deletions in the
first batch. Device-scenario migration not started.

## Why

The Python tools form one import cycle (finding 104), so Nx cannot select or cache Python tests by
owner. Most cross-owner imports and all expensive Python gate time come from the Python
device-testing stack: scenario runners, harness and proofs. Moving that stack to TypeScript deletes
it; the remaining Python tests stay as they are.

## Triage of the 253 Python test files (about 58 000 lines)

| Destination | Files | Lines | Content |
| --- | --- | --- | --- |
| Keep in Python | about 203 | about 53 000 | Python SHARC core, transpiler and generators (source of truth of the Rust core), hardware-measured facts, dev tools on the gate path, firmware and RE analysis, Python patchkit logic and in-process core integration tests |
| Move to TypeScript device tests | about 25 | about 3 000 | `tests/digikit` scenario wrappers (kinds: upload, settings, menu, safe/cut, equiv, roundtrip, debug/card/crash), smoke, `tests/emulator/test_coupled_patched.py`, `test_bootstrap.py`, `tests/control/test_runtime_load.py`, the device tests of `tests/control/test_digi_client.py`, patchkit boot tests (`test_patchkit_dkrt_boot.py`, `test_patchkit_dkrt_machine.py`, `test_patchkit_expansion.py`, `test_patchkit_rust.py`, `test_modcheck_device.py`), `tests/sharc/integration/test_sharc_golden.py` |
| Duplicates | 0 whole files | | Rust covers individual functions only; the Python transfer and protocol code (`tools/device/dkupload.py`, `dksettings.py`, `dktransfer.py`) duplicates Rust and TypeScript implementations, not just tests |
| Delete | 2 files and 2 functions | | `test_pk_proofs.py` (argv pins), freqshift `test_runtime_routing.py` (router), and the two functions whose assertions Rust repeats (`codec.rs` rebuild, `dkmdsp.rs` effect record) |

Kept after review: `test_pk_types.py` (type-number registry contract), `test_sharc_dsp_facts.py`
(59 values captured on hardware), `test_pcm_pin.py` (PCM pins need Em's approval).

## Order

1. Migrate the device scenarios family by family, as the FX work did: assertion ledger, TypeScript
   test on the shared helpers, proof on both devices, then delete the Python test, its scenario
   TOMLs and the runner code only it uses. Start with upload.
2. When the last Python consumer of `dkupload.py`, `dksettings.py` and `dktransfer.py` is gone
   (`transfer-cli.ts` runs the Python CLI today), switch to the TypeScript or Rust implementation and
   delete the Python one with its tests.
3. Tests of pure analysis tools used by hand (for example `tests/sharc/analysis`, coldfire analysis,
   DSP analysis, probe tooling): proposed for deletion. A scout first separates them from tools that
   production code imports or mise build tasks run (those keep their tests).
4. Measure the Python tool graph again (finding 104) after steps 1 and 2.

## Migration status (2026-11-02)

TypeScript ports exist for upload (all but upload-rust), settings, roundtrip, equiv, card-safety,
card-small, debug-log (in `debug-module.test.ts`), debug-guard, crash-capture, safe-fault,
safe-late-fault and cut-selection. Shared helpers: `card-kit.ts` (cards, guest reads, mailbox),
`card-rig.ts` (one reusable card fixture with the common close checks), `write-safety.ts` (zero-copy
cards: a hard link to a shared base plus `overlay.bin`), `device-data.ts`, `fx-live.ts` (PCM).
Each file has a `runtimeAcceptance` record, and a module's device tests run with
`pnpm nx:pilot run <module-id>:test-device`. Proof state and open items:
[handover](../handovers/HANDOVER-2026-11-02-scenario-migration.md). No Python scenario is deleted yet;
upload-cut and upload-clear-cut are skipped in TypeScript until the overlay issue is fixed.
