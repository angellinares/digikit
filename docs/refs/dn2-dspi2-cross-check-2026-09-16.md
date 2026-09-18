# The DSPI2 frame on Digitone II 1.11 — a cross-device check of your findings

**2026-09-16.** We worked the ColdFire↔SHARC link on **Digitone II 1.11** and
then read your `machine-engine-link` `FINDINGS.md`. You are ahead of us on this
and your account is better than ours — eDMA channels 28/29, CTAR0 `0xFA010000`,
TCD 29 DADDR → `PUSHR`, the 16-pass per-track frame loop. **Two things we
initially concluded were wrong and your notes corrected them**, which is
recorded in our repo as such.

So this is not a re-statement of your section. It is the **other instrument**
run against the same routines, plus one comparison you cannot make from a
Digitakt alone.

Everything below is Digitone II OS 1.11, MAIN OS (section 3) at `0x40000400`.

---

## 1. The frame is the same shape on the DN2, with one length different

Your DT2 call is `FUN_400cf9c4(0x802, 0x80005348, 0xabc, 0x8000488c)`.

Ours, at `0x40025e9e` on the DN2:

```
0x40025e8a  pea 0x800053a4     ; rx_buf
0x40025e90  pea 0xabc          ; rx_len = 2,748
0x40025e94  pea 0x80005e60     ; tx_buf
0x40025e9a  pea 0xa80          ; tx_len = 2,688
0x40025e9e  jsr 0x400cf7be
```

| | **DN2 1.11** | DT2 1.15C/1.16 (yours) |
|---|---|---|
| `tx_len` | `0xa80` = **2,688** | `0x802` = **2,050** |
| `rx_len` | `0xabc` = **2,748** | `0xabc` = **2,748** |
| second (test-mode) handler | `tx 0xa80`, `rx 0` | `tx 0x802`, `rx 0` |

**The receive length is identical across the two instruments; the transmit
length is not.**

We think that is a real constraint on your open question about whether the
frame carries an engine id. What the ColdFire *sends* is device-specific — which
fits per-track machine state, of which the two devices have different amounts —
while what the SHARC *returns* is a fixed 2,748-byte block common to both. A
field whose meaning is device-independent is more likely to live in the return
block; a machine/engine selector, if it exists, is in the 2,688 vs 2,050.

It also says something for anyone trying DT2 machines on DN2 hardware: the
transmit frame would have to be re-shaped, the return path would not.

The second handler matches yours exactly in structure — DN2 `0x400d0fec`,
`(0xa80, 0x4244098c, 0, 0)`, with the header word `2` written to
`0x4244098c` immediately before the call (`movew #2` at `0x400d0fe6`). Its
buffer is in BSS rather than SRAM, as yours is at `0x429307f4`.

## 2. Address correspondences, DN2 1.11 against your DT2 names

Offered so you can carry anything you have on one to the other.

| Your DT2 name | DN2 1.11 | Note |
|---|---|---|
| `FUN_400cf9c4` DSPI2 driver | `0x400cf7be` | |
| `FUN_400cef6c` SHARC boot routine | `0x400cf34c` | |
| its two `jsr` sites `0x4002d6ba`, `0x400d13d4` | `0x40025e9e`, `0x400d0fec` | |
| ELE3 header compare at `0x40128b8c` | `0x40134546` | `movel #'ELE3',d0` |
| — | `0x4013459a` | `find_section_by_id(id, entry[16])` |
| — | `0x4013458a` | `section_data_address(entry) = offset + 0x80000` |

DN2 1.11 has **six** section-table entries where your trace shows five on the
DT2: ids 5 (meta), 2 (bootstrap), 3 (MAIN OS), 4 (updater), 7 (blob), 8.
Entry layout confirmed from the id-8 caller: `+0 id`, `+4 offset`, `+8 stored
length`, `+12 dest`.

## 3. The boot upload on the DN2 is PIO, not eDMA

Different routine from the runtime driver, and worth stating because the
mechanism differs. `0x400cf34c` reads section 7 by id, mallocs 1 MB, copies the
stored bytes in, writes a `0x03` block header, then pushes **one byte at a
time**, spinning on `TCF` rather than using eDMA:

```
0x400cf60a  moveb 0xec094018,%d0    ; flow control, bit 4
0x400cf616  bnes 0x400cf60a         ; spin while asserted
0x400cf618  mvzb %a0@+,%d1          ; next byte
0x400cf61a  oril #0x90010000,%d1    ; CONT=1, CTAS=1, PCS0
0x400cf620  movel %d1,0xec038034    ; PUSHR, directly
0x400cf626  movel 0xec03802c,%d0
0x400cf62c  bges 0x400cf626         ; spin on SR bit 31 = TCF
0x400cf62e  movel #0x80000000,%d0
0x400cf634  movel %d0,0xec03802c    ; write-1-clear
0x400cf63c  movel #0x18000000,%d1   ; EOQ frame closes the queue
```

Note `CTAS=1` here selects **CTAR1**, whereas your runtime driver's
`FUN_400cf67c` sets **CTAR0** to `0xFA010000` for 16-bit frames. So boot and
run use different attribute registers — 8-bit byte pushes at boot, 16-bit
frames afterwards. Consistent with ADI's SPI-slave boot expecting a byte
stream.

## 4. `0xec09xxxx` mapped, in case it saves you the pass

Not a data path — a byte-wide FPGA register file. **≥279 accesses across 57
addresses** in `0xec094000`–`0xec094070`, 257 of them byte-wide:

| Address | Accesses | Shape |
|---|---|---|
| `0xec09404e` | 25 | read/write, **7 functions** |
| `0xec094018` | 21 | 8 functions, incl. the boot flow control above |
| `0xec094024` | 18 | **write-only**, 7 functions |
| `0xec09404b` | 15 | 6 functions |
| `0xec094019` | 13 | 4 functions |
| `0xec094034` | 8 | the only exclusively **word-wide** one, only in the boot routine |
| `0xec09406x` | 1–4 | all in `0x400cec70` — one subsystem's block |

Lower bounds: we match `move`/`movea`/`clr`/`tst` whose absolute address
follows the opcode word, and deliberately not `andi`/`bset`, whose immediate
comes first.

## 5. An independent confirmation of your `movclr` point

You noted Ghidra lists no callers for `FUN_400cf9c4` or the boot routine,
because the stock ColdFire language cannot decode `movclr`, and warned that an
empty caller list is not evidence of dead code.

Confirmed from the other direction: our caller scan is `m68k-elf-objdump
-m m68k:cfv4e` over a 2-byte stride, and it finds both DN2 call sites without
trouble. Both handlers open with `movel %macsr,%a0` / `movclrl %acc0,%d0` and
so on, which is how we recognised them as ISRs. So the gap really is the
decoder, not the image.

Two corrections of our own in the same area, since they are the same class of
error and you may have picked either up from us:

- We had concluded **"no upload path in MAIN OS"**, and from that, that the
  SHARC boots from its own serial flash and its program was permanently out of
  reach. Wrong — it was a negative from searching the `0xec09xxxx` window,
  where the channel is the DSPI next door.
- We first mapped that window as "220 accesses across 50 addresses". Also
  wrong: the opcode mask matched only the `d0` spelling of each `move`, missing
  `movel %d1,0xec038034` — the boot data port itself. Worth a glance if you
  have a similar scanner; the register number is part of the opcode.

---

## Thank-you note on the flash question

Your "the SPI flash needs no physical dump" section answered an open question
of ours the same day we wrote it down. We had found the container lookup and
its `entry.offset + 0x80000`, and had assumed `0x80000` was a RAM address the
staged package had to stay resident at — with a hardware trace queued to check
it. It is a **flash offset**, read through `read(offset, len, dest)`, and your
boot trace shows the section-table scan and the section 7 read directly. That
closed it for us without the trace, and changed the answer for the better.

Happy to re-run any of the above against 1.16, or to hand over the DN2 1.11
extraction in whatever form is useful.
