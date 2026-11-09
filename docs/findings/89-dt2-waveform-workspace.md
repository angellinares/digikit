# DT2 waveform-summary workspace ownership

2026-10-29, DT2 OS 1.17. **[D]** inspected evidence; **[O]** remaining obligations.
This is a bounded ownership follow-up for the [sampler plan](../plans/dsp-sdk-and-sampler.md),
not a new allocation implementation. No emulator or hardware run was performed.

**[D]** An independent audit checked 240 aligned instructions across three ranges
against original section-7 bytes. Blob SHA-256 matches database provenance:
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`.

| Routine | Supported behavior |
| --- | --- |
| `0x1c4547..0x1c4568` | Forward bump allocation using count at `0x82a00008`: compute count plus request R4, compare unsigned against `0x800000`, return zero on excess; otherwise store new count and return `0x80a00008 + 4 * old_count`. Nominal pool is `[0x80a00008,0x82a00008)`, exactly 32 MiB. |
| `0x1c4703`, continuation `0x1c4820` | Rebuild cached summaries when a three-field identity differs from `0x82a001b8/bc/c0`; reset allocator count at `0x1c4761`, initialize descriptors at `0x82a00010` and optionally `0x82a000e4`, and call `0x1c45aa`. |
| `0x1c45aa` | Allocate packed signed-16-bit minimum/maximum pairs, then combine adjacent pairs into progressively coarser levels, capped below 24 levels. This is summary construction, not a general free list. |
| `0x1c483d`, `0x1c4440` | Check the same identity and query the descriptors; use direct minima/maxima for small ranges and coarser summaries for larger ranges. |

The numerical algorithm is strongly supported; interpreting it as a sample
waveform overview is an inference from its data and callers.

**[D]** These are recurring operations. Task creation loads entry `0x1c3bf0` at
`0x1c3f65` and calls creator `0xb8615d` at `0x1c3f6a`. The task has a persistent
dispatch loop at `0x1c3c1d`; calls at `0x1c3dc7` and `0x1c3e79` reach the rebuild
and query routines, then return to the loop. A sample bank placed inside this
workspace without changing its ownership would be vulnerable to later rebuilds.

**[D]** Loader FILL block 102 covers `[0x80a00000,0x82a001c4)`. Under valid counter
state and nonoverflowing requests, the audited allocator grows upward from
`0x80a00008` and does not allocate down into the earlier experimental bank gap
`[0x8055e890,0x80a00000)`. This bounds this allocator only. It does not establish
that the lower gap is unused, owned by us, or safe on hardware.

**[O]** Static reservations, startup configuration, other pointer origins and
indirect writers still need an ownership account before a production bank can be
reserved. Shrinking or partitioning this stock workspace has not been implemented or
evaluated for compatibility. The stock project sample base `0x8422b9c8` remains a
separate path; these DT2 addresses must not be transplanted to DN2.

**[D]** Source-quality limits: heuristic function boundaries split real
continuations into separate database functions. One descriptor-index instruction
at `0x1c4664` remains rendered as an undecoded form. Neither changes the audited
allocator arithmetic or observed min/max loop structure.
