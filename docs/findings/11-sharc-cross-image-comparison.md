# SHARC cross-image structural comparison

This is a bounded static comparison of three section-7 loader streams:

| image | blob SHA-256 | recovered functions | decoded PCs | indirect sites | pointer runs |
| --- | --- | ---: | ---: | ---: | ---: |
| DT2 1.16 | `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2` | 1,229 | 50,976 | 1,282 | 9 |
| DN2 1.11 | `336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2` | 1,156 | 54,156 | 1,221 | 11 |
| DN2 1.10E | `174b391822bbe33a99e5f42bd02ef3ea75c0e79351caae6ba90e4cd2c4de5350` | 1,150 | 53,371 | 1,216 | 10 |

All results are single-review structural evidence, so they are **[D]** rather
than **[V]**. They do not identify a live audio root or establish that two
functions have the same semantics.

## Bounded inventories

The DN2 manifests select nine non-fill loader blocks each. Selection uses the
same loader-target code regions as the existing DT2 inventory: the common
`0x282403f0` initialization region, one small `0x282d...` block, the
`0x20000000` L2 region, the three small `0x2838004c`/`0x283801a4`/
`0x28380328` regions, startup at `0x28380548`, and the two main-code regions
ending at `0x28382720`. The block indices differ where the loader stream's
preceding records differ. The manifests record each selected block's loader
address and payload size; no root, runtime path, or product-specific role is
asserted. **[D][O]**

The generated reports and SQLite indices live under ignored `out/` paths.
`tools/sharc_compare.py` consumes reports only and emits canonical JSON. It
does not open firmware, use cache paths, or include timing, jobs, PID, or
input ordering in output. Missing optional evidence is reported as
`unavailable`, not inferred. **[D]**

## Structural overlap

The comparison signature contains instruction count, a fixed numeric decoder
feature subset, and direct/resolved/unresolved call counts. It excludes
addresses, table literals, identifiers, labels, and runtime claims. Matching
signatures are prioritization evidence only: collisions and independently
implemented functions with the same shape are possible. **[D][O]**

- DT2 1.16 versus DN2 1.11 has 1,001 function instances in common signature
  buckets, with 228 DT2-only and 155 DN2-only instances. Thus a large shared
  structural substrate exists, but this count is not a one-to-one function
  map. **[D]**
- Of those buckets, 740 are collision-free one-to-one candidates under this
  signature, and 60 candidates retain the same short-word entry address in
  both images. The largest same-address candidate is the 430-instruction
  startup-region function at `0x1c02a4`; its structural label is block
  copy/move and its signature includes one indirect call. This is a shared
  platform-initialization lead, not an audio-root identification. **[D][O]**
- DN2 1.10E versus 1.11 has 1,119 common instances, with only 31 1.10E-only
  and 37 1.11-only instances. This same-product control shows that release
  skew alone changes a bounded part of the recovered inventory. It has 857
  one-to-one candidates, including 193 at the same short-word entry address,
  and gives a baseline for interpreting DT2/DN2 differences. **[D]**
- The DN2 1.11 image has more decoded PCs than DT2 1.16 despite fewer
  recovered functions. Counts therefore must not be read as simple feature
  or complexity rankings. **[D]**

## ISA-frontier context

Across the selected code blocks, the typed decoder sees 51 forms in DT2 1.16
and 52 in DN2 1.11. The DN2 image adds form `13a` to this bounded inventory.
The current blocker forms occur in both images: Type14d appears 80 times in
DT2 and 54 times in DN2 1.11, Type11a 5 and 7 times, and Type7d 28 and 20
times respectively. Cross-image recurrence supplies useful fixtures and
context, but it is not independent encoding authority because the products
may share compiler and firmware ancestry. **[D][O]**

## Byte-identical wavetable stages across DT2 1.16 and DN2 1.11 **[V]**

An exact-byte check of the six wavetable stages in
`docs/findings/06-sharc-engine-and-startup.md` ("Reading the DSP"), separate
from the signature-bucket method above. Both blobs were read with
`tools/sharcldr.LoadedMemory`:

| stage | DT2 1.16 `sw` (bytes) | DN2 1.11 address | match |
| --- | --- | --- | --- |
| 1 | `0x1ccbd8` (256) | L2 byte `0x200026d0` | 256/256 |
| 2 | `0x1cdecb` (218) | L2 byte `0x20004cb6` | 218/218 |
| 3 | `0x1cb3d8` (436) | L1 `sw 0x1cdb56` | 434/436 |
| 4 | `0x1cd286` (606) | L2 byte `0x2000342c` | 606/606 |
| 5 | `0x1cc79e` (814) | L2 byte `0x20001e5c` | 809/814 |
| 6 | `0x1cbf07` (626) | L2 byte `0x20000dea` | 623/626 |

Stages 1, 2 and 4 are byte-identical. Every differing byte in stages 3, 5
and 6 has one of two causes:

- A `Type8a` delayed `CALL` whose PC-relative offset differs only because
  the instruction sits at a different address. `tools/sharcflow.pcrel_target`
  resolves all of them (stage 3 at DT2 `sw 0x1cb3ff` / DN2 `sw 0x1cdb7d`;
  stage 5 at DT2 `sw 0x1cc7f5` / DN2 `sw 0xb80f85`; stage 6 at DT2
  `sw 0x1cbf4d` / DN2 `sw 0xb8073b`) to the same target, `sw 0x1c06ba`, the
  shared reciprocal helper.
- One `17a` table-pointer literal in stage 5 (destination `I2`): `0x26bb68`
  in DT2 (`sw 0x1cc85d`), `0x26b3a8` in DN2. The table itself differs; the
  code that reads it does not.

The helper at `sw 0x1c06ba` (`0x1c06ba`-`0x1c0729`, 222 bytes) matches for
its first 160 bytes (the `RECIPS` and Newton-Raphson body) and differs in 58
of the last 62, its compiled epilogue (return-address load, conditional
branch, `RFRAME`/return from `sw 0x1c070a`).

**[C]** The call graph settles it. `out/sharcdb/dn2-1.11.sqlite` has a function
`sw 0x1c9b73`-`0x1c9c9d` (126 instructions, no static `CALL` caller) reached by
exactly one edge, `cond_jump 0x1c99a8` (`JUMP IF SZ`, cond 8) from inside a
larger function. It calls stages 1 (`0xb81368`), 2 (`0xb8265b`), 4
(`0xb81a16`), 5 (`0xb80f2e`) and 6 (`0xb806f5`, twice): the same stage set as
`FUN_1c71ec`. **[V]**

The larger function, `sw 0x1c8ef1`-`0x1c9b73` (1352 instructions, called from
`0x1c3044` inside `sw 0x1c2712`), calls stages 1, 2 and 3 (`0x1cdb56`, twice)
from around `sw 0x1c959f`. Together they mirror DT2's
`FUN_1c642a`/`FUN_1c71ec` pair, joined by a conditional jump rather than a
call. `sw 0x1c2712` has a 16-iteration `DO...UNTIL LCE` loop at `sw 0x1c2937`,
like `FUN_1c2b24`, and is called from `sw 0x1c9fbc` inside `sw 0x1c9e76` (159
instructions, no static caller). **[V]** `func_hash` matches `sw 0x1c9e76` to
DT2's `FUN_1c75d8` by relocation-tolerant hash, and it calls `0xb891fa` where
DT2 calls `0xb8615d`, but its arguments are computed from `DM(0x268a38)`
rather than immediates, so that correspondence is **[O]**.

## Next boundary

The generated comparison now retains the addresses and heuristic labels of
collision-free one-to-one shape candidates separately from the normalized
signature. The next comparison should move to conservative normalized
instruction and call-subgraph identities, then intersect those matches with
IVT, SPORT/DMA/PCG, scheduler, mixer/effects, and output evidence.
DN2-specific FM regions should remain a bounded contrast set; DT2 remains the
primary hook target. Runtime identification of the active vectors and audio
chain is still required. **[O]**
