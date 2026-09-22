# Per-function SHARC notes

One file per function, named `<block>-<sw-address>.md` (e.g. `blk93-1c18a6.md`).
One function per file so parallel readers never conflict.

`tools/sharcinv.py` inventories **1228 functions** across the nine SHARC code
blocks. These notes record the ones that have actually been *read*. The
inventory gives a feature vector for every function; a note here means someone
decoded it and understood what it computes.

Marks follow the repo convention: **[V]** verified here (a second agent checked
it against the image bytes), **[D]** read once, not re-checked, **[O]** open.

## Phase B notes

- [`blk93@0x1c4ecf`](blk93-1c4ecf.md) is the callable entry and
  [`0x1c4f81`](blk93-1c4f81.md) its internal continuation. **[V]**
- [`blk93@0x1cbdea`](blk93-1cbdea.md) is a wrapper **[D]** and
  [`0x1cc6b8`](blk93-1cc6b8.md) a shared zero/store leaf **[D]**.
- [`blk93@0x1c18a6`](blk93-1c18a6.md) has a bounded, cross-image RAM-word
  forwarding interval **[V]**; its runtime role remains open **[D][O]**.

## Template

```markdown
# blk93@0x1c18a6 — <one-line name for what it does>

- **Bounds**: entry `0x…`, exit `0x…`, N instructions (matches / differs from inventory)
- **Callers**: … **Callees**: …
- **Label**: <inventory label> — agrees / disagrees, and why

## What it computes

<The algorithm, named, with the instructions that show it. If two readings
fit, give both and what would distinguish them.>

## Structure

<Block-by-block: prologue, each loop with its real trip count (say whether
literal `12a_imm` or register `12a_ureg`), branches, calls, epilogue.>

## Memory

<Buffers and tables: address, stride, element size, read/write. Note any
touch of a named table or the audio rings.>

## Interface

<What arrives in R4/R8/R12 and the I/M registers; what is written back.>

## Open

<What could not be established, and what would settle it.>
```

## Conventions that cost people time here

- **Address spaces differ by block.** blk93/blk1/blk88 use
  `byte = 0x28000000 | (2 * sw)`. **blk69 does not** — its target is
  `0x20000000` (the L2 byte base), so `base_sw = 0xb80000`. State which you are
  using.
- **Scaling is per instruction form.** `19a` (`modify`) adds its immediate as
  raw bytes; `19a_scaled` scales by normal-word; `dm(K,I)` scales K by **4**
  under 32-bit normal words; M-indexed scales by 2 if short-word tagged, else 4.
  Determine the form before converting, and quote the instruction.
- **`tools/sharc_trace.py` models float compute** as of this session. Run it
  with `--blob --start <sw> --concrete-memory --assume-32bit-normal-words
  --follow-loaded-calls --summary`.
- A function can sit **inside** another's return-delimited span, so a direct-call
  target landing inside a span opens a function there.
- Per-instruction decode confidence does **not** distinguish code from data;
  desync frequency does.
