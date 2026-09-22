# blk93@0x1cc6b8 — shared zero/store leaf

- **Bounds**: `0x1cc6b8`–`0x1cc6c4`, 7 instructions. **[V]**
- **Static callers**: `0x1cbd58`, `0x1cbdea`, and `0x1cc6f2`. **[V]**
- **Label**: inventory `glue/trampoline`; consistent with this tiny shared
  leaf. **[D]**

## What it does

It moves `R4` to `I4`, sets `I12=0`, stores that zero through `DM(I4,M5)`,
loads `DM(I6,M7)` into `I12`, and returns. **[V]** The destination object,
stride, and semantic role of the store are **[O]**.

## Evidence

Deterministic dossier:
`out/sharc-engine/phase-b/blk93-1cc6b8.txt` (DT2 1.16
`section_7_BLOB.bin`, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`),
matching the verified source blob. **[V]**
