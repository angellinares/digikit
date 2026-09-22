# blk93@0x1cbdea — state/update wrapper

- **Bounds**: `0x1cbdea`–`0x1cbe19`, 23 instructions. **[V]**
- **Callers**: three static call sites, `0x1c6d08`, `0x1c6dca`, and
  `0x1c6e9d`, occur in the same `blk93@0x1c642a` caller function.
  **Callee**: direct delayed call at `0x1cbdfb` to `blk93@0x1cc6b8`.
  **[V]**
- **Label**: inventory `unclassified/mixed`; this is a wrapper, not a compute
  kernel. **[D]**

## What it does

The wrapper saves `I15` at `I6+126` and `R2` at `I6+125`, sets `M4=0x44`,
stores `M14` through `DM(I5,M4)`, and stores `M13` at `I5+14`. After the
delayed call, it copies
`I5+10` to `I5+3` and `I5+4`, copies `I5+7` to `I5+8`, restores, and returns.
**[V]** The object and pointer meanings remain **[O]**.

Why the three static wrapper call sites exist is **[O]**.

## Evidence

Deterministic dossier:
`out/sharc-engine/phase-b/blk93-1cbdea.txt` (DT2 1.16
`section_7_BLOB.bin`, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`),
matching the verified source blob. **[V]**
