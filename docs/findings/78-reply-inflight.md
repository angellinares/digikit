# 78. The reply record while it is sent: a check, and why the module step word arrives as 0 on a DT2

Trigger: on a real Digitakt II (1.17) the module step word, which the core delivers in word 298 of a fixed reply record (finding 77, `core/dsp-link`), was 0 in 3995 of
4000 blocks, while the emulator delivered it every second block. Marks as in AGENTS.md. Nothing here is **[V]**; the device has not run the check.

## 1. How the record is sent

- **[D]** The DSP's SPI2 is a slave (CTL `0x351`); the ColdFire's DSPI2 is the master and sets the clock. The transmit DMA (channel 26, base `0x3102D200`) runs in
  descriptor-list mode over two descriptors, one per record, so the DSP does not arm a transfer per block: when a frame has been sent, the DMA takes the next
  descriptor by itself (finding 48; `periph.rs dma_done`). The check read the registers at every exchange: see the `transmit DMA` line of its output
  (`SHARC_REPLY_INFLIGHT`, `docs/INSTRUMENTATION.md`).
- **[D]** One frame is 2748 bytes (1374 16-bit words, XMOD 2) in both directions on both devices; the records are `0x266220` and `0x267220` on the DT2 1.17 and `0x2c49d0`
  and `0x2c59d0` on the DN2 1.12 (the DMA sees them in the short-word alias, `0x28266220` and so on).
- **[D]** The ColdFire side (`FUN_400cd0d8` in DT2 1.17, `FUN_400cf4b0` in DN2 1.12, the boot-time DSPI2 setup) writes CTAR0 `0xFA010000`: 16-bit frames, PBR 1 (divide by 3),
  BR 0 (divide by 2), DBR 1 (double), so SCK is the module clock divided by 3, with no delays. With the firmware's own 132 MHz bus-clock constant (finding 08, open
  point: the DSPI module clock itself is not read from a register) that is 44 MHz; at 125 MHz it would be 41.7 MHz.
- **[D]** So the wire time of one record is 21,984 bits / 44 MHz = **0.50 ms**, 75 % of the 0.667 ms block. Word 298 (byte `0x4a8`) is shifted out 0.217 ms after the
  frame starts. The DMA's FIFO read-ahead is not known; the check treats a word as read when its first halfword would be shifted out.
- **The emulator** (`Engine::spi2_exchange`) takes the TX work unit's bytes at the instant the ColdFire frame arrives, then runs the DSP for the whole period. A store
  into the record during that period never changes what was sent, so the model cannot show a race.

## 2. The check

`SHARC_REPLY_INFLIGHT=1` (or `fail`): from each exchange for the wire time, the engine compares the record with a snapshot after every dispatch unit (the same hook as
`SHARC_WWATCH`; one flag test per unit, off by default) and reports each changed word with PC, unit letter and EARLY or late, as defined in `docs/INSTRUMENTATION.md`.
Limits: changes of value only; exact PCs for the interpreter (patched code always runs there), the entry PC for block units; the phase between the DSP block and the
frame is the model's, not a measurement.

## 3. Result on the current scheme (step beacon, remote and probe selected; `sharc-probe.test.ts`, `SHARC_REPLY_INFLIGHT_WORDS=298`)

Every second exchange sends the fixed record (the one that receives the module's patched stores). In that exchange the same three writers change word 298, in this
order, every time:

| step | DT2 1.17 | DN2 1.12 | what |
| --- | --- | --- | --- |
| 1 | PC `0xbce504`, +197,436 | `0xbecf50`, +291,631 | `_fx_mark` clears the step word in both records (value to 0) |
| 2 | `0xb8d93d`, +197,592 | `0xbc449d`, +291,787 | the module's patched store (0 to the step value) |
| 3 | `0x1c28fb` (block), +208,178 | `0x1c25cc` (block), +305,839 | stock code that rebuilds the record clears the word again (value to 0) |

Positions are DSP instructions after the exchange (1 GHz); the DMA reads word 298 at +216,727. Counts: DT2 1985 changes per PC in 3969 exchanges; DN2 1085 per PC in 3026.

- **DT2:** all three come before the read, so with this phase the wire carries 0 whatever the module stored: the final value before the read is the stock clear. The
  emulator delivers the value because it reads the record at +0, before any of the three, and the store of the earlier block (made while the other record was sent,
  when the stock code does not touch this record) is still there.
- **DN2:** all three come after the read (+292,000 to +306,000), so the wire carries the value stored in the earlier block. This phase explains why the failure was
  reported on the DT2 only; it is the model's phase on both, not a device measurement.
- So the hypothesis fits: on the DT2 the stored step is erased by the stock rebuild of the record while the DMA has not yet read the word. The 5 blocks of 4000 that
  arrived would be blocks where the handler ran later in the frame. **[O]** The real phase between the frame start and the handler is not measured; the check
  shows the order of events, not the device's timing.
- A fix must put the value in the record after the stock rebuild and not later than the DMA read of word 298, or into the record that is not being sent and not rebuilt in
  that block. Neither is tried here.

## 4. Stock firmware

With no module the stock DT2 1.17 changes no word of the record before the DMA read after its boot (the test `tests/regression/runtime/reply-inflight.test.ts`, `fail`
mode, DT2 only: a stock DN2 daemon in snapshot mode made no SPI2 exchange in 12 s of emulated time). On the DN2 the patched runtime with the step beacon showed only late changes. Findings: DT2 1.17 writes words 679 to 686 (the record's last 32 bytes) at PC `0xb88e47` (an envelope or gain function) during exchanges 1788 and 1789 of
a boot, early; the test skips the first 2500 exchanges. Stock also changes words 0 and 5 of the record late in each exchange (DT2 `0x1c75ad`, DN2 `0x1c9e4b`): after the
DMA has read them.
