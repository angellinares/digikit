# SHARC execution addresses, and which of the blob is code

Measured on **Digitone II 1.11** (section 7, 836,956 bytes) and cross-checked on
**1.10E** (833,060 bytes), 2026-09-13. Contributed from
`angellinares/dn2_firmware_explore`.

**Depends on the FILL-bit fix in PR #2.** With `FILL_BIT = 12` the walk stops at
11,248 bytes and none of this is reachable; with `FILL_BIT = 8` the chain
consumes the section exactly and the whole image loads.

## 1. `load = exec * 2 + 0x28000000`

Boot-stream `target_address` fields are **load** addresses (`0x2826ee10`).
`sharcscan.py`'s cjump targets are **execution** addresses in the `0x1c` and
`0xb8` spaces. They are different numbers for the same bytes, and nothing can be
cross-referenced until they are related.

Two pieces of evidence:

**The entry point lands on a block boundary.** The final block of the stream
(flags bit 15) gives entry `0x001c12e2`. Mapped, that is `0x283825c4` --
**exactly the first byte of a loaded code region**, to the byte. An arbitrary
affine map does not do that.

**The population agrees.** Of 1,616 cjump-absolute targets, **591 of 591** in the
`0x1c` space land inside a loaded region under this map:

| candidate map | targets inside a loaded region |
|---|---|
| `t * 2 + 0x28000000` | **591** / 1616 |
| `t * 4 + 0x28000000` | 0 / 1616 |
| `t + 0x28000000` | 0 / 1616 |

The factor of **2** is the interesting part: code addresses count **16-bit
words**, which is VISA's instruction granularity -- and independently agrees with
`sharcldr.py`'s own `alignment()` finding (repeated motifs on even offsets only,
spread uniformly across mod 4, 6 and 8). Two unrelated measurements, same fact.

### Open: the `0xb8` space

**1,025 of the 1,616 targets** are in the `0xb8` space and this map puts them
outside every loaded region, so **no mapping is known for it**. It may address
DDR -- the image reserves 5.4 MB at `0x80000000`, almost all of it fill, which is
where a large working set would sit -- but that is a guess and is recorded as
one.

## 2. Which regions are code

`sharcscan.py`'s decode, applied per loaded region (Digitone II 1.11):

| region | bytes | cjump | |
|---|---|---|---|
| `0x20000000..0x2001e880` | 125,056 | **1002** | **code** (L2) |
| `0x2001e888..0x2008823c` | 432,564 | 0 | data |
| `0x28240000..0x28240300` | 768 | 0 | |
| `0x282403f0..0x2826f000` | 191,504 | 0 | rodata |
| `0x282c0000..0x282dd52c` | 120,108 | 0 | rodata |
| `0x28380000..0x283825c0` | 9,664 | 10 | |
| `0x283825c4..0x2839bffc` | 105,016 | **604** | **code** (L1) |
| `0x80000000..0x80000014` | 20 | 0 | |
| `0x80000018..0x8052fbe0` | 5,438,408 | 0 | DDR, almost all fill |

**~240 KB of the 837 KB section is code**, and the hits concentrate rather than
spread. The 432 KB data region and the 5.4 MB DDR fill returning zero is the same
negative control this project already uses (49,152 bytes of float coefficients ->
zero hits), here obtained without arranging for it.

**[C][D]** The L1 and L2 labels in the table above were the wrong way round when
this file was merged, and are corrected here. `0x28xxxxxx` is L1 and
`0x20000000` is L2, which is what `docs/sharc/structure-1.16.md` and
`docs/sharc/SPEC-FINDINGS.md` already say. Section 1's own evidence settles it:
`load = exec * 2 + 0x28000000` is a short-word to byte alias, and the SHARC+
Core Programming Reference documents that aliasing as a property of L1, which is
four independent blocks each mapped to its own region of the address space (Rev
1.5, chapter 7, "L1 Memory Interface"). The sub-bases seen here -- `0x2824xxxx`,
`0x282Cxxxx` and `0x28380000` -- are those blocks. L2 is one instance, L2CTL0, a
single unified SRAM with the boot ROM rather than four blocks (ADSP-2156x SHARC+
Processor Hardware Reference, Rev 1.0, chapter 8, "L2 System Memory"), which
matches the single `0x20000000` region. The addresses, the counts and the
formula are unchanged.

**1,616 against your 1,656 for Digitakt II** over a comparable region -- two
products, two extractors, same result. The decode transfers.

## 3. Landmark strings, for naming code by file

The blob carries unstripped `__FILE__` strings from FreeRTOS's `configASSERT`,
plus a task name. FreeRTOS is open source, so code referencing one of these is
inside a file whose source can be read upstream.

| exec address | string |
|---|---|
| `0x13453c` | `Audio Task` |
| `0x137708` | `..\..\..\..\lib\freertos-sharc\event_groups.c` |
| `0x137728` | `..\..\..\..\lib\freertos-sharc\queue.c` |
| `0x137760` | `..\..\..\..\lib\freertos-sharc\stream_buffer.c` |
| `0x1377a0` | `..\..\..\..\lib\freertos-sharc\portable\CCES\SHARC_215xx\port.c` |
| `0x16839c` | `..\..\..\..\lib\freertos-sharc\tasks.c` |
| `0x168424` | `..\..\..\..\lib\freertos-sharc\timers.c` |
| `0x16e870` | `..\..\..\..\lib\freertos-sharc\portable\heap_4.c` |

There are also three ADI service-driver asserts naming `adi_gpio.c:226`,
`adi_pcg_v1.c:178` and `adi_spu_v2.c:147`, with ADI's build host path.

**A warning that cost us time.** We tried to find the referencing code by
scanning for these addresses as 32-bit words. It finds **zero** -- at load
addresses and at exec addresses, stride 1, both endiannesses, across all 6.4 MB.
The addresses are never materialised as a plain word. Why is **unexplained**; the
leading candidate is that VISA builds them across instruction fields, but that is
a hypothesis, not a measurement, and we had to walk back stating it as a cause.
**A word scan cannot do this job** -- it needs `sharc_disasm.py`.

## Reproducing

`bootstream.load_regions()` in dn2_firmware_explore plays the stream into the
memory it describes (9 regions, 6,423,108 bytes with fills materialised), then
the counts above are one pass of the cjump decode per region. Nothing here
disassembles SHARC.
