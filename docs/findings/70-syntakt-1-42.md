# Syntakt OS 1.42: container, CPUs and the peer link

All claims are **[D]** unless marked **[O]**. Plan:
[firmware-decls-and-sdk](../plans/firmware-decls-and-sdk.md).

## Container

- File: `Syntakt_OS1.42.syx`, ELE3, 2,029,486 bytes. The META stamp is
  `260930 11:17:25`, the same day as DT2 1.17 and DN2 1.12.
- sha256: `ded3c59fdb909eb9e2c3307b30a486452c8fa6947b12c14e0ce35a187b7f37c1`.
- Extract: `uv run python -m tools.firmware.extract firmware/Syntakt_OS1.42.syx -o out/sections/st-1.42`.
- The SysEx ids (transport id 0x16, OS-stream byte 0x11) are in
  [finding 01](01-container-and-patching.md).

| Section | Content | Size |
| --- | --- | --- |
| 5 | META | |
| 2 | bootstrap, dest word 0x04000000 (DT2/DN2: 0x02010000), ColdFire code at 0x8000xxxx | 15,408 packed, 30,782 |
| 1 | absent on DT2/DN2. 0xFF padding, then `aa 99 ...`, bitstream-like | 49,252 packed, 149,516 |
| 3 | MAIN at 0x40000400, same startup words as DT2 | 1,442,812 packed, 3,442,576 |
| 4 | UPDATER at 0x80000400 | 32,768 raw |
| 6 | absent on DT2/DN2. A 5-entry `trapf; jmp 0x8000xxxx` table plus SPI helper code. Load address probably 0x80000000 **[O]** | 1,744 raw |
| 7 | DT2/DN2: packed SHARC loader stream. Here: a ColdFire program, see below | 383,760 raw |
| 8 | byte-identical to DT2/DN2 section 8 | |

- The packing codec is the same as on DT2/DN2.
- The content checksum of DT2/DN2 reproduces the stored value over the first
  2,029,392 bytes. The last 94 decoded bytes are zero padding.
- There is no HMAC trailer. The last 32 bytes are zero. The sha256 constants
  are absent from sections 2, 3 and 4.
- The per-packet checksum constant K = 0x11 verifies on all 20,094 data
  messages.
- Consequence: `Device::from_id` in `packages/patchkit/src/syx.rs` needs a
  no-seal path for the Syntakt. There is no key to derive.

## Images and CPUs

- MAIN is MCF5441x ColdFire code with the same reset path as DT2: reset entry
  0x400004e8, SP 0x48000000, flags word read from 4(sp).
- Section 7 is a standalone ColdFire program linked at 0x40000400. The first
  long, 0x40001070, is the entry. The code there has the same reset prologue
  as MAIN. 43 of 46 absolute `jsr`/`jmp` targets land right after an `rts` at
  that base.
- It drives a register block at 0xFC05C000 with a DSPI-like layout (+0x2C
  status, +0x34 push, +0x38 pop; identity **[O]**), plus EPORT and GPIO.
- MAIN has no reference to 0x40001070 and no copy of section 7.
- Which CPU runs section 7 and how it is loaded: **[O]**. It is consistent
  with a second ColdFire that runs the digital machines **[O]**.

## FPGA

- MAIN does not configure the FPGA. In the boot thread `FUN_40082578`,
  `_DAT_454e4c14 = DAT_4029a778 >> 12 & 0xf` at 0x40082798. `DAT_4029a778` is
  the bootstrap flags word.
- The self-test screen `FUN_40080646` (0x40081e62) decodes the value: 1 INITB
  never went high, 2 INITB low during data, 3 INITB low after data, 4 DONE
  never went high. This is Xilinx-style INIT_B/DONE configuration, so the
  bootstrap configures the FPGA.
- The bootstrap uses peripherals that the DT2 bootstrap does not use:
  - DSPI at 0xFC03C000 (MCR 0x80030c00), at 0x80001242-0x80001276.
  - An eDMA TCD (0xFC0451E0..0xFC0451FE) that feeds PUSHR 0xFC03C034 from
    buffer 0x80007af0, at 0x80001066 (file offset 0 = 0x800003ec).
- That this transfer carries the bitstream: **[O]**.

## Peer link ("DSP boot")

- Status word `_DAT_41bbf448`, read by `FUN_40082a50`. Values: 0 WAIT, 1 init
  fail, 2 TIMEOUT, 3 BUFSIZE, 4 DSP_MEM, 5 CF_MEM, 6 OK.
- Poll callback `FUN_400828cc`: in state 0 it counts ticks. Above 0x12 it sets
  status 2.
- Handshake handler `vector_68_handler` (0x40083034), EPORT IRQ4, cleared by
  writing 0x10 to 0xFC090006.
- Doorbell `FUN_40082f6a` toggles GPIO PA bit 4 (PPDSDR_A = 0x10, PCLRR_A =
  0xEF).
- Mailbox in the FlexBus window at 0x10000000. State variable: 0x41bb32b0.
  - Peer magic at 0x10000000: 0x484F ("HO") is hello. 0x4E4F ("NO") sets
    status 4.
  - 0x10000004 must read 0xA5A5 in state 5 for status 6.
- `FUN_400829f4` sends progress bytes.
- `FUN_40082f90` copies 2 KB chunks to 0x10000800. The sequence number is at
  0x1000000C and command 2 is at 0x10000002. The source is a buffer at
  0x4d84f2c1 (RX buffer 0x4d84f2c0 or 0x4f84f2c0: **[O]**).
- `vector_149_handler` (0x400831d6) re-arms DSPI2 (0xEC038000) with eDMA
  channel 29 for 0x6000 bytes into that buffer.
- Consumer: the main OS task at 0x4000abd6. It shows "DSP BOOT FAILURE
  (TIMEOUT)" at 0x4000af8c, "(CF)" for 5, "(DSP)" for 4, "(BUF)" for 3
  (strings at 0x4024bd6a-0x4024bdc3).
- Peer state names at 0x4025303e-0x402530df: STATE_LOAD_OS, INIT, BOOT,
  RUNNING, UPLOAD_SAMPLES; FAILURE_TIMEOUT, BUFSIZE, CF_MEM, DSP_MEM;
  BOOT_SUCCESS.
- Whether the peer is the section 7 ColdFire: **[O]**. The exact reply
  sequence: **[O]**.
- To boot in the emulator, stub: the hello magic and 0xA5A5 on the window,
  IRQ4, and DSPI2 eDMA channel 29 data.

## Anchors on 1.42

Dry run of every cf `[[anchor_rule]]` (172 rules, 128 names) against an
in-memory registered `st-1.42`: 27 hit (15.7%), 145 miss, 0 ambiguous.
Controls with the same harness: DT2 1.17 111/111, DN2 1.12 101/101.

| Kind | Hit | Miss |
| --- | --- | --- |
| cf_func | 22 | 115 |
| cf_ptr | 0 | 19 |
| cf_words | 2 | 5 |
| cf_params_* | 0 | 6 |
| cf_reset_* | 3 | 0 |

| Module | Hit | Miss |
| --- | --- | --- |
| core/expansion | 3 | 0 |
| core/runtime | 9 | 15 |
| core/digikit-menu | 6 | 11 |
| core/health | 4 | 8 |
| core/machines | 2 | 67 |
| core/input | 1 | 2 |
| core/fxpages | 1 | 23 |
| core/dsp-link | 0 | 8 |
| core/dynreg | 0 | 2 |
| core/params | 0 | 8 |
| master-drive | 0 | 1 |

- A hit only means the hash is unique on 1.42. 17 of the 25 hits that also hit
  on DT2 have identical bytes at the anchor. Semantic sameness: **[O]**.
- `cfmatch match dt2-1.17-emac st-1.42-emac` (no `--write`) pairs 9,966
  functions: 67.0% of DT2 and 76.2% of Syntakt. Methods: strict 2,947, loose
  580, strings 200, vtable 2,280, callee 1,200, caller 375, gap 2,372.
- The caller and gap stages are wrong in places. Example: the DT2 boot thread
  0x400cc9c8 is paired with the Syntakt FPGA status code.

## Emulator boot notes from OS 1.41 [D]

- Source: an old Python-emulator boot of OS 1.41 (PR #42, not merged). Measured
  once, not verified on 1.42. Re-derive every byte signature on the 1.42 image.
- The 1.41 boot reached the main screen ("A01") after about 331 M instructions
  with no FancyErrorView.
- There was no DSP model. The DSP loader gave up at about 690 M instructions
  and the UI then showed "OOPS". The audio-frame interrupt was not modelled.
- The OS busy-waits on DTCN2 (DMA timer 2 counter, loop at 0x40000f78 on 1.41).
  The bootloader leaves the timer running and the OS never writes DTMR2. The
  emulator must serve DTCN2 as a free-running counter.
- Worker pends that must stay real. A faked release ran jobs that were never
  posted and starved lower-priority tasks (1.41 names from the PR):
  - `pump_wait2`: second worker pump, prio 7. Waits on a job semaphore.
  - `hw1_wait`, `hw2_wait`, `hw3_wait`: three prio-5 hardware workers (SPI NOR
    via GPIO 0xEC0940xx; a FlexBus device at 0x10000000; a copy worker). The PR
    does not say which name is which worker. Each waits on a semaphore posted
    by an ISR.
  - `display_frame_wait`: the display task's per-frame wait on display_sem,
    posted by the display PIT3 ISR. Faked, the task spins at 100% CPU.
- Panel wire (measured by use, 1.41):
  - Button message `2c mask`: `mask` is the whole state of channel c.
  - Encoder message `3c dd`: `dd` is a signed delta.
  - Channel 0 bits 0-7 = tracks 1-8. Channel 1 bits 0-3 = tracks 9-12.
  - Channel 2 bit 7 = LFO page. Channel 3 bit 3 = NO.
  - Encoders A-H = channels 0-7. LEVEL = 8.
  - The first encoder message after a page or parameter change only shows the
    value. Each later one moves by delta/6 positions.

## Analysis databases

- Ghidra project: `out/ghidra-projects/st-1.42-emac` (language
  `68000:BE:32:ColdfireEMAC`, separate from the 1.16 project).
- Dump: `out/ghidra/st-1.42-emac`. cfdb: `out/cfdb/st-1.42-emac.sqlite` with
  13,073 functions, 1,007 typeinfos and 12 decompile failures.
- Seeds and RTTI come from `codeseeds` and `rttiscan` with
  `--code 0x40000400 0x401d0000 --code 0x40336000 0x40338000`. Without these
  ranges there are 9,498 functions.
- Pipeline: `tools/coldfire/ghidra.sh` import + `McfLabels.java`, `codeseeds`,
  `rttiscan`, `ghidraapply seeds --analyze`, `ghidraapply rtti`,
  `ghidradump`, `cfdb build`, `cfdb fts`.
- `ghidradump` and `ghidraapply` need pyghidra, which the project venv does
  not include. The run used
  `uv run --offline --with pyghidra==3.1.0 --with jpype1==1.5.2`.
- Section 7 is not imported yet. `ghidra.sh` base 0x40000400 fits it.
- There is no a Syntakt device file in devices/ (not yet written) yet. `port_resolve`,
  `tools/coldfire/regions.py` `IMAGES` and `cf.load` need a registered name.

## Open questions

- Who runs and loads section 7.
- The mailbox reply semantics.
- Which bootstrap code streams section 1.
- Whether 0xFC05C000 is a DSPI.
- How many ColdFire anchors can be re-derived from cfmatch strict and vtable
  pairs.

## Scope

- Support is limited to the ColdFire images.
- Section 1 and the bootstrap are never modified.
- The FPGA is never emulated or reconfigured (risk to the analog circuitry).
  The emulator may stub the FPGA status and the peer interface only.
- Plan: [firmware-decls-and-sdk](../plans/firmware-decls-and-sdk.md).
