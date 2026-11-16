# DYN-DN2-PAGES (test)

A Digitone II machine for the digikit runtime with two parameter pages: PGA (ALPH, BETA, GAMA) and PGB (EPSL, ZETA). Derived from `test/dyn-dn2`, which has one page and stays unchanged (the dynreg DN2 PCM pin).
Proof: `modules/core/runtime/tests/machines/machine-pages-dn2.device.test.ts` (the SYN key cycles the pages; each knob edits its own parameter).
Pack it with `uv run python -m tools.patchkit.patchkit pack test/dyn-dn2-pages --device dn2 -o FILE.dkm`.
