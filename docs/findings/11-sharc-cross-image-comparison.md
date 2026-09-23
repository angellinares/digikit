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

## Next boundary

The generated comparison now retains the addresses and heuristic labels of
collision-free one-to-one shape candidates separately from the normalized
signature. The next comparison should move to conservative normalized
instruction and call-subgraph identities, then intersect those matches with
IVT, SPORT/DMA/PCG, scheduler, mixer/effects, and output evidence.
DN2-specific FM regions should remain a bounded contrast set; DT2 remains the
primary hook target. Runtime identification of the active vectors and audio
chain is still required. **[O]**
