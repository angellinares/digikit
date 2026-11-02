# DYN-DN2 (test)

A Digitone II machine for the digikit runtime: a data-only module with one machine (`own_dsp`, type 16), four parameters and one
SYN page with four knobs. Pack it with `uv run python -m tools.patchkit.patchkit pack test/dyn-dn2 --device dn2 -o FILE.dkm`.
Proof: `tests/regression/runtime/equiv.test.ts` (boot-time load; the hot `pk_registry` proof is archived). Design: `docs/design/digikit-registry.md`,
results: `docs/findings/33-runtime-machines-dn2.md`.
