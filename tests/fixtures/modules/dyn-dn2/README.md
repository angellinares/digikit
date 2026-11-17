# DYN-DN2 (test)

A machine for the digikit runtime on both devices: a data-only module with one machine, four parameters of different ranges (Alpha 0..127, Beta 0..63, Gamma 1..127, Delta 0..127) and, on the Digitone II, one
SYN page with four knobs (`own_dsp`, type 16). The DSP is the one of `test/sine`. Pack it with `uv run python -m tools.patchkit.patchkit pack test/dyn-dn2 --device dn2|dt2 -o FILE.dkm`.
Proof: `modules/core/adapters/dynreg/tests/equiv.device.test.ts` (boot-time load; the hot `pk_registry` proof is archived). Design: `docs/design/digikit-registry.md`,
results: `docs/findings/33-runtime-machines-dn2.md`.
