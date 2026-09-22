# blk93@0x1c4ecf — callable entry for a conditional path

- **Bounds**: return-delimited entry span `0x1c4ecf`–`0x1c4f81`, 84
  instructions. Its restore/return epilogue is `0x1c4f56`–`0x1c4f7c`.
  **[D]**
- **Caller**: Within the stated aligned/depth>=8 decoded-block coverage, one
  known direct call is the unconditional `0x1c6b00 -> 0x1c4ecf` call from
  `blk93@0x1c642a`; this bounded coverage does not claim global exclusivity.
  **[V]**
- **Control flow**: conditional jumps at `0x1c4f25` to
  [`blk93@0x1c4f81`](blk93-1c4f81.md) and at `0x1c4f2c` to `0x1c4f56`; the
  latter is the shared restore/return path through `0x1c4f7c`. The
  continuation jumps from `0x1c5264` to `0x1c4f56`. **[V]**

## What it does

The save-state and resampling interpretations are **[D]**. The continuation
inherits `I4`; the broad statement that reads at offsets `+98` through `+109`
are relative to that context is **[D]**, while the context's concrete identity
is **[O]**.

At that call, the exact local chain is `0x1c6ad3: I4=DM(I6+124)`,
`0x1c6ad7: R2=4`, `0x1c6ad9: R1=DM(I6+124)`,
`0x1c6adb: R13=R1+R2`, and `0x1c6afd: R4=R13`. The exact conditional gate
targets are `0x1c6acc -> 0x1c6530`, `0x1c6aeb -> 0x1c6afd`, and the
back-edge `0x1c6b09 -> 0x1c6ae8`; the `R14=0x20` setup and its decrements
are byte facts. The incoming `I6` root/object identity, natural branch
outcomes, and effective loop trip count remain unresolved. Thus this entry is
neither shown to select a machine nor to execute universally (including per
track). **[V][D][O]**

Exact phase fields, SRC-page ownership, and context/object identities are
**[O]**.

## Evidence

Deterministic dossier:
`out/sharc-engine/phase-b/blk93-1c4ecf.txt` (DT2 1.16
`section_7_BLOB.bin`, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`),
matching the verified source blob. **[V]**

Reproducible bounded extraction: `out/sharc-engine/machine-path/summary.txt`
(SHA-256 `fb7a6a080015e4ce173f745dde17160aa5970931b85ad2a2919a3a22cb5301e4`);
the two identical extraction records hash to
`73c5ce0ad83f7bb0e92156841f27b06aa7a74fe808d2f29848756ba1bf98b218`.
**[D]**
