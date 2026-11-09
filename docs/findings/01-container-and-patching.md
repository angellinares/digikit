# Container, patching and integrity

Scope of the analysis, the 2.01-generation firmwares, the ELE3 container format, its integrity checks, the version gates, the recovery/bootstrap path, and what in the image is safe to patch.

Target SHA-256 `62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c`
(matches the target of `lalzart/digitakt-ii-firmware-research-public`).

## Scope, and the 2.01 firmwares **[V]**

Everything address-specific in this file is **Digitakt II 1.15C**, whose MAIN
OS is `sections/section_3_MAIN_OS.bin`, sha-256 `6a6a887b…`. The snapshot
ladder and the Ghidra program are the same image. The container and transport
results — both checksums, the HMAC and its key derivation, the framing
message's transfer constant and message count — are the exception: those are
confirmed byte-exact against all four firmwares in the repo root.

`Digitakt_II_OS1.16.syx` and `Digitone_II_OS1.11.syx` are a different
generation, and three things separate them from 1.15C/1.10E:

- **A sixth section, id 8**, packed, **103,416 bytes compressed on both
  devices** — byte-for-byte the same compressed length on Digitakt and
  Digitone, which suggests a shared component rather than per-device content.
  Decompressed, both are the same 159,948 bytes. Nothing else is known about
  it. **[V][O]**
- **The bootstrap version bumps, `0x0200` -> `0x0201`.** Section 2's `dest` is
  the version word, and it reads `0x02000000` in 1.15C and 1.10E, `0x02010000`
  in 1.16 and 1.11. So installing either of the newer firmwares performs the
  bootstrap upgrade — the one irreversible operation on the device, and the
  reason `tools/patchkit/patchimg.py` refuses section 2 outright.
- **The Unicorn depacker cannot read them.** Every packed section of 1.16
  fails under `tools.firmware.extract --oracle`. The cause is the oracle, not the new
  section: the depacker is taken from the UPDATER, and 1.16's UPDATER differs
  from 1.15C's by **43.4%** (14,231 of 32,776 bytes, first difference at
  offset `0x9b`), so the entry point at `0x80000432` has moved. **[V]**
  `tools/firmware/elz.py`, a byte-level decoder that `tools.firmware.extract` now uses by default,
  reads them. On 1.15C and 1.10E it matches the device routine byte for byte
  on every packed section; on 1.16 and 1.11 every stream ends exactly at its
  declared length. **[V]**

Retargeting the machine work to 1.16 is therefore not an address rebase. It
needs a fresh snapshot ladder built by cold boot, a re-import to Ghidra, and
every address in "The ColdFire machine dispatch" re-derived. **[O]** The
re-import is done; see "Digitakt II 1.16 in Ghidra" below. **[V]**

## Container

`.syx` → SysEx transport (13,346 × 128-byte messages, `F0 00 20 3C 14 00 …`)
→ 8-in-7 bit decode → 8-byte preamble (content checksum at +4) → ELE3
container, 1,347,728 bytes, five aPLib-compressed sections. Nothing encrypted. **[V]**

| id | name | decoded | dest / meaning | what it is |
|---|---|---|---|---|
| 5 | meta | 15 | — | build stamp `250910 15:18:30` |
| 2 | "DSP" | 30,302 | `0x0200` = **version**, loads `0x80000400` | **the bootstrap** — misnamed in the tool |
| 3 | MAIN OS | 3,177,312 | `0x40000400` | the C++ application |
| 4 | updater | 32,776 | `0x80000400` | stored raw, not compressed |
| 7 | blob | 320,780 | — | SHARC ADI loader records |

Section 2's `dest` is not a load address. All 101 absolute call targets in it
land in `0x8000____`; solving for the base that makes its pointer table hit real
string starts gives `0x80000400` with 96 hits. **[V]**

The above is the offline OS-**upgrade** container. The device's *runtime* SysEx
command surface on 1.16 — the live MidiRpc protocol (Ping, version/UID queries,
`FsSample*` file I/O, the `OsUpgrade*` flash channel) and the SDS handler — is a
separate topic, in `docs/MIDI-SYSEX-RPC.md`. A live device round-trip is
confirmed there; the static decode is **[D]** pending a second byte-check. **[D]**

## Integrity — not a barrier to patching

- Per-packet transport checksum; 32-bit content checksum; **HMAC-SHA256** trailer.
- No RSA/ECDSA anywhere. The HMAC **key is derived from material inside the
  firmware itself**, not stored — same code at `0x80005d90` in both devices,
  only the data differs. For a per-device STRING and the 32-byte CONST stored
  immediately after its NUL: **[V]**

      key[i] = CONST[i] ^ sha256(STRING)[i] ^ sha256(STRING[::-1])[i]

  | device | STRING | CONST at |
  |---|---|---|
  | Digitakt II | `"Master Overdrive"` @`0x80006ff8` | `0x80007009` |
  | Digitone II | `"Multiplier"` @`0x8000706c` | `0x80007077` |

  The "32-byte constant beginning `69 5d 82 bc`" earlier notes describe is only
  one of the three XOR operands, not the key. **[C]**
- **All three integrity fields are recovered and computed**, each confirmed
  byte-exact against all four firmwares in the repo root. **[V]**

  | field | where | algorithm |
  |---|---|---|
  | content checksum | preamble bytes 4-7 | `sum(i ^ word_i)` over 1-based big-endian u32 words of the whole container, trailer included |
  | HMAC trailer | container's last 32 bytes | HMAC-SHA256 over `container[:total_len-32]` |
  | per-packet | message byte 125 | `(K + sum(body[6+i] ^ (i+K), i=0..118)) & 0x7F` |

  The container ends with 16-byte alignment padding then the 32-byte trailer,
  all inside `total_len` (`1347692+4+32 = 1347728`, and the same on the other
  three). The per-packet checksum had previously resisted an exhaustive search
  over 30,603 pairs — it is not a CRC or a hash but folds each byte's own
  index in, a family that search never covered. **[C]**
- The transport carries no unknown fields. `K` above is byte 7 of the 16-byte
  framing message (`0x0F` Digitakt II, `0x10` Digitone II), and framing body
  bytes 11..13 are the **data-message count** as a 21-bit base-128 value.
  Blanking those counts and discarding the source preamble, re-encoding
  reproduces all four firmwares byte-identically. **[V]**
- Round-trip is lossless: extract → rebuild → re-extract returns all five
  sections byte-identical, checksums and HMAC verifying. The rebuilt `.syx` is
  *not* byte-identical (the tool's aPLib packer beats Elektron's by 80,880
  bytes) and **that does not matter** — see below. **[V]**

## Byte-identical packing is unnecessary

The device's own depacker at `0x80000432`, run under Unicorn, decompresses both
Elektron's original streams and the tool's repacked ones to **identical
SHA-256**, all three compressed sections including the full 3.1 MB MAIN OS.
Nothing in the acceptance path hashes the original compressed bytes. **[V]**

Reproduce: `./venv/bin/python emu/oracle.py`

## The version gate

Upgrade routine begins `0x80001c48`. The gate is at the top, before anything is
displayed or written:

```
80001c66  mvz.w  -$4(a6), d3        ; INCOMING bootstrap version
80001c6a  mvz.w  $80000408.l, d0    ; RUNNING bootstrap version (= 0x0200)
80001c70  cmp.l  d3, d0
80001c72  bcc.w  $800021dc          ; current >= incoming -> EXIT, no upgrade
```

`bcc` is unsigned ≥, so BOOTSTRAP UPGRADE runs **only** when the incoming
version is strictly greater. Re-flashing 1.15C over 1.15C never triggers it. **[V]**

`0x71F9` is `mvz.w`, a ColdFire-only opcode Capstone cannot decode — a naive
sweep desynchronises directly on top of this instruction and reads the gate as
garbage. Use Ghidra's `68000:BE:32:Coldfire` or `tools/coldfire/disasm.py`.

The later "VERSION CHECK" screen at `0x80001e52` is a milder equality check that
the decompressed image declares the version its header claimed. **[V]**

### MAIN OS has a second gate, and it reads the BUILD string **[V on 1.16]**

The open question below -- *"never reads the container version string at an
absolute address, so the comparison was not located"* -- is answered, and the
reason it was not found is that **it does not read the version string at all,
and the container is in a register.**

Read on **Digitakt II 1.16**, not 1.15C, so the addresses are for 1.16 and the
recipe for locating it on 1.15C is below.

The MAIN OS upgrade receive state machine hands its validator `%fp@(32)` and
switches on a return of 1..6; 6 is `Unsupported downgrade` / `Downgrade not
possible`. On 1.16 the validator is `0x400d9e4c`:

```
400d9e54  jsr     0x40120a70         ; content checksum      -> 0 : return 3
400d9e60  move.l  (a2), d0           ; stream length
400d9e62  move.l  #$3030362F, d1     ; "006/"
400d9e68  cmp.l   $10(a2), d1        ; the image's BUILD string, ELE3 +0x08
400d9e6c  bcs.s   0x400d9e76         ; "006/" < build -> continue
400d9e6e  moveq   #6, d0             ; else -> Unsupported downgrade
400d9e76  jsr     0x400d0588         ; HMAC-SHA256 trailer   -> false : return 4
400d9e88  moveq   #1, d0             ; pass
```

`'/'` is `0x2F`, one below `'0'`, so *"strictly greater than `006/`"* is how the
compiler wrote **"build >= 0060"**.

Three consequences:

- **It is a hard-coded floor, not a comparison.** Nothing reads the running
  firmware's version. This is the opposite of the bootstrap gate above, which
  compares incoming against running. So MAIN OS does **not** reject a
  same-version image -- `0079` over `0079` passes, and so does every build at or
  above `0060` whatever is installed.
- **It reads the build string at ELE3 `+0x08`, not the version string at
  `+0x13`.** A search for the version string finds nothing.
- **`%a2` is the 8-byte stream preamble, reached through a register**, which is
  why no absolute reference to the container exists. The validator's argument is
  `state + 32` where `state` is the SysEx decoder state: `%a2@` is the stream
  length, `%a2@(8)` is the container, `%a2@(16)` is the build string. The
  decoder state's address comes from a one-instruction accessor, so the whole
  chain is register-relative.

**To locate it on 1.15C**, or on any sibling: search MAIN OS for

```
22 3c ?? ?? ?? ?? b2 aa 00 10      # move.l #<4 ASCII bytes>,d1 ; cmp.l $10(a2),d1
```

That found it in 3 of the 4 images checked, with no false positives.

The floor is not a barrier to reinstalling Digitakt II 1.15C after 1.16. The
BUILD string at container `+0x08` is `0071` for 1.15C and `0079` for 1.16, read
from the two `.syx` files with `tools/firmware/container.py`, and the floor is `"006/"`, so
1.15C clears it by eleven builds. The bootstrap version gate above is a separate
mechanism, and it is the one that does not allow going back. **[V]**

### It is per-product, and two products do not have it **[V]**

Same validator slot, same six-entry error table (`No error`, `Checksum failed`
x3, `Power adapter must be connected`, `Unsupported downgrade`), in every image
checked:

| image | error table | validator | build floor |
|---|---|---|---|
| Digitakt II 1.16 | `0x4021f6fc` | `0x400d9e4c` | **`"006/"`** |
| Digitone 1.43 | `0x401ce9f0` | `0x400a003c` | **`"0022"`, `"0025"`, `"0072"`** |
| Digitone II 1.11 | `0x40208748` | `0x400dbc4c` | **none** |
| Syntakt 1.41 | `0x40242c98` | `0x400a5ba8` | **none** |

Only two of those four rows were re-checked here against image bytes. This repo
has sections for Digitakt II 1.16 and Digitone II 1.11 but none for Digitone
1.43 or Syntakt 1.41, so those two rows are **[D]**, carried from PR #15 and not
independently verified. For Digitakt II the validator, the `"006/"` constant,
the `cmp.l $10(a2),d1` and the error-table strings were read from the bytes. For
Digitone II `FUN_400dbc4c` is 52 bytes, calls only a checksum and a trailer
routine, and no path returns 6, so its `Unsupported downgrade` string is present
but unreachable. **[V][C]**

Digitone 1.43 selects between its three floors on bit 19 of a global at
`0x402292f0`, almost certainly Digitone vs Digitone Keys -- the two variants
sharing that firmware. Not chased. **[O]**

Digitone II 1.11's validator is 52 bytes, does the checksum and the trailer and
nothing else, and returns only 1, 3 or 4. Its `Unsupported downgrade` string is
present and **unreachable**. Worth stating plainly because the string was read
as evidence of the behaviour there first, and that was wrong: the error table is
referenced from exactly one site and nothing writes 6.

### `Incompatible OS` is a product check, not a version check **[V]**

Adjacent, and a cheaper mistake to avoid. The SysEx packet parser rejects a
header packet whose **byte 8** is not the product's own OS-stream id, and the
message is `Incompatible OS`. Byte 8 is not the transport device id at byte 4:

| product | byte 4 (transport id) | byte 8 (OS-stream id) |
|---|---|---|
| Digitakt II | `0x14` | `0x0f` |
| Digitone II | `0x15` | `0x10` |
| Syntakt | `0x16` | `0x11` |
| Digitone 1 | `0x0d` | `0x08` |

So `Incompatible OS` means "another machine's firmware", and never fires on a
version.

### The HMAC trailer is verified on the device **[V]**

Worth recording next to "Integrity -- not a barrier to patching", which it does
not contradict but does sharpen: the check is not only in the vendor's tooling.
MAIN OS's validator calls it on every upgrade, and on Digitone II 1.11 -- where
the key derivation was read end to end -- it derives key material from the
string `"Multiplier"`, digests the container minus its last 32 bytes, and
compares against those 32 bytes. A rebuild that does not carry a correct trailer
is refused with `Checksum failed`, not with a distinct message.

## Recovery

The bootstrap owns the STARTUP menu (`0x8000650d`), the factory test mode, and
`READY TO RECEIVE` (`0x80006603`) — the legacy MIDI-DIN upgrade path. It is
independent of MAIN OS and validates only:

1. content checksum — `sum(i ^ word_i)` over 1-based big-endian u32 words,
   at `0x80003ca6`. Earlier notes give this as `0x40003ca6`; that is a
   transcription slip — `0x80003ca6` is the instruction that reads the length
   word the checksum covers, `move.l (0x40000000).l,D2`. **[C]**
2. HMAC-SHA256 trailer (`0x80005e2a`; SHA-256 H0 at `0x800058bc`, K-table at `0x80006ef8`)

**No version comparison on this path.** On failure: "UPGRADE ABORTED" and a spin
loop, nothing written. **[V]** That a corrupt MAIN OS still lets the menu come up
is **[V]** below (spike 0.2, 2026-10-13: read statically, checked by a second agent
from the raw bytes, and run on the real bootstrap code, see "Bootstrap on the
ColdFire core").

The receive path itself, traced in the bootstrap: each message's 101 decoded
bytes are written to `0x40000000 + seq*101`, the message count comes from the
framing message with **no bound check**, and the erase/write loop to flash
offset `0x80000` caps nothing either — no software size limit exists anywhere
on this path. **[V]**

The two classes of failure behave very differently. A bad byte-125 checksum
sets `_DAT_80008e3c`, which is written in six places and **read in none** —
the receive state machine silently resets, with no message. A bad content
checksum or HMAC branches into `FUN_80003bfc`, which prints "UPGRADE ABORTED"
/ "PLEASE REBOOT" and hangs in an infinite loop that never returns, so the
erase/write loop after it is unreachable. **[V]**

```
80003748  move.b (0x80007d17).l,D4b   ; K, the transfer-type const (0x0F here)
80003752  move.b (0x0,A3,D0*1),D5b    ; body[6+i]
8000375c  eor.l  D5,D2                ; ^ (i + K)
8000375e  add.l  D2,D1                ; running sum
8000376e  mvz.b  (0x78,A2),D1         ; body[125], the stored checksum
80003774  cmp.l  D1,D0                ; against (K + sum) & 0x7F
```

## What is patchable

MAIN OS holds parameter state and RPCs it to the SHARC — shared structs appear
under a `Digisharc` namespace (`sound_struct`, `fx_setup_struct`,
`logicalParamID_t`, `modTarget_t`, `rpcMsgHeader_t`, `kitStorage_v*`). Machines
and filters are labels and parameter IDs on the ColdFire; audio runs on the DSP. **[V]**

- **247,996 bytes (7.8% of MAIN OS) in 1,392 contiguous string tables.** Machine
  and filter names (table at `0x4022b20e`: `WERP`, `STRETCH`, `REPITCH`,
  `SLICED SMP`/`SLIC`, `STATE VARIABLE`/`SVAR`, `LOWPASS 4`/`LP4`, `EQUALIZER`,
  `COMB-`/`COMB+`, `LEGACY LP/HP`), 24 KB of factory sample paths, mod sources,
  song sections, the random-project-name word list. Same-length editable. **[V]**
- Constants, ranges, defaults; size-preserving ColdFire logic.
- **Adding a new machine**: the ColdFire side is no longer the blocker — see
  "The ColdFire machine dispatch" below. Section 7 is a real ADI loader stream;
  the remaining blocker is that there is **no SHARC assembler or semantic
  model**, so a genuinely new algorithm still means flash-and-listen on
  hardware. A machine that reuses an existing DSP mode with different
  parameters avoids that entirely. **[V]/[O]**

## Digitakt II 1.16: rebuild, and cave space that survives a cold boot **[V]**

`tools/firmware/build.py` rebuilds 1.16 after two changes: packed sections are
decompressed with `tools.firmware.elz.depack_section` (the oracle depacker's entry
point moved on 1.16), and section 8 is stored like 2/3/7. A rebuild with no
replacements re-extracts to six sections byte-identical to the stock
extraction, and `tools/firmware/roundtrip.py`'s `check_authenticity()` passes
(preamble checksum, HMAC trailer, framing count, 42508/42508 packet
checksums). A one-byte replacement in section 3 survives the round trip as
exactly that byte.

The reset path zeroes most of the tail of the MAIN OS image, so a cave that
reads as zero in the image is not necessarily free. From the reset entry
`0x400004e8`, two unconditional calls run before the OS:

- `jsr FUN_4000045c` at `0x4000053e` copies `[0x40312000,0x40318e80)` and
  `[0x40318e80,0x4031ff60)` to the SRAM window at `0x80000000`.
- `jsr FUN_400004b2` at `0x40000542` zeroes `0x40312000..0x47e28470`:
  `207c 40312000` (`movea.l #0x40312000,a0` at `0x400004ba`),
  `223c 47e28470`, then `clr.l d4-d7; movem.l d4-d7,(a0); lea 16(a0),a0;
  subq.l #1,d1; bne`.

1.15C has the same code with start `0x402fa000`, end `0x47e0f2c0`.
`cave_b` (1.16 `0x4031be5c`, 1.15C `0x40303e5c`) lies inside the cleared
range on both, so bytes flashed there are gone before the OS runs; it only
worked for live patches of a resumed snapshot. `cave_a` ends exactly at the
clear start. Checked by a second agent against the image bytes, and at
runtime: a patched 1.16 image lost its `cave_b` bytes by the 60M rung of a
fresh boot and kept its `0x403117c4` bytes.

`tools/cavefind.py` (lost; `tools/coldfire/regions.py` replaces it, see "RAM map" below)
finds caves that survive boot: it reads the copy and
clear ranges from the reset code by signature, takes runs of 0x00/0xFF below
the clear start, rejects any run with a pointer literal into it (or a table
starting just before it), rejects runs whose bytes differ in any saved
snapshot rung, and `--confirm SYX` boots a canary-filled image to 60M.
Results, as `machineprofile` `flash_cave`: DT2 1.16 `0x403117c4`, 2108 B;
DT2 1.15C `0x402f9c14`, 1004 B; DN2 1.11 `0x402fb7a8`, 2136 B. On 1.16 all
1306 canary words across the seven surviving candidates were intact at 60M.
The rejected runs include `0x4031041c` (3045 B, referenced by
`lea $40311000` and `move.l #0x4031041c,d3`) and eight 0xFF runs of
1024-1376 B, each a live buffer with one reference.

From the MCF5441x reference manual: CACR is written once, `0xa50ce100`
(caches enabled and invalidated in the same write), and ACR0 once,
`0x4007e020` (`0x40000000-0x47ffffff`, copyback cacheable, not
write-protected); there is no ACR1-3 write. Flashed cave code is in SDRAM
before the caches are enabled, so it needs no cache maintenance.


## Bootstrap on the ColdFire core (spike 0.2) **[V]** unless marked

Section 2 (DT2 1.16 and 1.17 are byte-identical, version 0x0201) loads at 0x80000400
(the file's first 4 bytes are a length word; the image's first two longwords are SP
0x80010000 and PC 0x80000de8). `packages/emulator/bootstrap` runs its real instructions on the
native ColdFire core; `tools/emulator/bootstrap_run.py` drives it; `tests/emulator/test_bootstrap.py`
pins the results. Modelled, not executed: the QSPI transport (flash read 0x8000f010,
flash id 0x800024ec, sector erase 0x80002714, page program 0x8000227e), the panel UART
(key banks, OLED records), the MIDI UART (one receive interrupt per byte), PLL, DDR
and PIT status bits. No timing. The boot-mode stack argument (1 = normal; 0 runs a
debug-register write) comes from a stage outside section 2, so the model passes 1. **[O]**

### Order of events from reset (static, second agent agrees)

1. 0x80000de8: PLL, copy flash routines to SRAM 0x8000f000, vectors, then by mode:
   mode 1 calls the init routine 0x800009c6 and only then 0x80000596 (load MAIN OS).
2. 0x800009c6 runs the bootstrap-upgrade routine 0x80001c48 (0x80000aea), scans the
   panel key banks (0x80001276, request `0x60 0x01`), and tests key index 0x11 with
   0x80001302. The bit comes from the table at 0x80006d74; key 0x11 = FUNC (names at
   0x80006ca8), bank 2 bit 0. If FUNC is held, or the upgrade routine returned
   non-zero, it calls the STARTUP MENU 0x80002dc4 (0x80000c60). Two other key chords
   (5 and 3 keys, checked at 0xc2a and 0xd92) set flag bits and skip the menu. The two
   chords (tested at 0x80000c2a and 0x80000d92) are evaluated only when the upgrade
   routine 0x80001c48 returned non-zero; with a stock return of 0 only FUNC decides.
3. 0x80000596 finds section 3 in the flash container table (0x80020 + 0x10*i), copies
   it to 0x40400000, depacks it (0x80000432) to 0x40000400 and 0x80000ef4 calls through
   the first longword. **Nothing checks a checksum or HMAC of MAIN OS** (the compares in
   0x800053ec are the container magic 'ELE3' and 0x2b, the Digitakt II value; the
   Digitone II bootstrap compares 0x34, `moveq #0x34` at 0x80005424 **[C]**), and a missing section is ignored.
4. So the FUNC test, the menu and the whole receive path precede any use of MAIN OS:
   a MAIN OS that faults at its first instruction leaves the menu reachable.
   Emulated: with such an image in flash, no key gives `fault vector 4 at 0x40000404`;
   FUNC held gives the menu (0x80002dc4) and 0x80000596 is never reached.
5. The menu is not an endless loop: codes 0x19, 0x1a, 0x1b, 0x1d exit (flag bits in
   0x80008404, boot continues); the OS UPGRADE entry is key code 0x1c (TRIG 4).

### Receive path, emulated end to end

FUNC held, TRIG 4 at 0x800013ac, the stock 1.16 .syx fed over the MIDI UART (1,880,864
bytes, 540 M instructions, 8 s): content checksum (0x80003c9c) and HMAC (0x80005e3a)
pass, six 256 KiB sectors 0x80000..0x200000 are erased, 1,484,288 bytes programmed, soft
reset (0x80 to 0xec090000), reboot, MAIN OS depacked equal to section 3, entry
0x400004e8. Flash afterwards equals the container at 0x80000, 0xFF below 0x80000.
Refusals: one container byte flipped, preamble checksum kept: UPGRADE ABORTED before the
HMAC; one byte flipped and checksum recomputed: aborted at the HMAC; one transport byte
flipped: the per-message checksum fails, the receiver silently resets, the transfer never
completes. In all three nothing is erased or written. Recovery: flash holding the
faulting MAIN OS, FUNC, TRIG 4, stock .syx, reboot: stock MAIN OS runs. These are the
bootstrap's own verifier functions accepting an image rebuilt by `tools.firmware.build`/`tools.firmware.inplace`
and the patched test image (`tools/patchkit/patched_image.py`).

### Bootstrap-upgrade gate **[V]**

0x80001c48 runs on every boot, not only on a transfer. It looks up section 2 in the flash
container table; if that entry's version halfword (first two bytes of the 4th longword)
is <= the running 0x0201 it returns 0 at 0x800021dc before drawing or writing (compare
0x80001c66-0x80001c72). Otherwise it stages the section, checks a CRC residue and that
the image header's version equals the table's, and only then erases flash 0x40000 and
programs the new bootstrap (<= 0xf000 bytes). So both install paths (STARTUP menu, and
MAIN OS 0x4013a978, which only erases and writes from 0x80000 and never below) leave the
bootstrap alone for an image whose section 2 equals the running one; the only decision
is taken at the next boot. Emulated: stock boot and the receive path above never touch
flash below 0x80000. A table version 0x0202 over an unchanged section 2 shows
"BOOTSTRAP UPGRADE ... VERSION CHECK", writes nothing and forces the reduced menu (TEST
MODE, OS UPGRADE). MAIN OS has one more bootstrap writer, the factory UART shell command
`FULL UPGRADE` (0x400cae8c; string `#FULL_UPGRADE`; erases and programs 0x40000, the erase
by jsr 0x40137a5e at 0x400cb0f6/0x400cb0fc). **[V]**

## NOR flash map (spike 0.2) **[V]** (static reading, second agent agrees on addresses)

| flash offset | content | evidence |
|---|---|---|
| 0x00000-0x3ffff | serial-boot loader area, content unknown | **[O]** the bootstrap never touches it |
| 0x40000- | bootstrap image written by the bootstrap upgrade (<= 0xf000 B), one 256 KiB sector | 0x80001c48; MAIN `FULL UPGRADE` |
| 0x80000- | OS container: header 'ELE3' + 0x2b, section table at 0x80020; erase from 0x80000 in 0x40000 sectors, 0x200-byte pages | bootstrap and MAIN read and write |
| 0x380000 | 0x200-byte record of 2-letter tagged fields ("FH", "UU", "UI", ...), rewritten by MAIN (0x400ca4d6, 0x400ca5b2) | reads at 0x400ca396.. |
| 0x3c0000 | serial number: tag "SERI", 14 bytes, CRC residue 0xdebb20e3 (0x400caa42) | |
| 0x400000, 0x800000, 0xc00000 | probed once for magic 0x434f4b69; if found a 0xdd9714-byte copy to the eMMC and the sector is erased (0x400f04e2). The 0x40000/0x48000 values there are eMMC sector numbers | |

The two records at 0x380000 and 0x3c0000 are sector-protected: MAIN tests them with the
status command 0xE2 and unprotects with 0xE4 before writing, re-locks with 0xE3 (command
meanings are the S25FL-S convention, an inference). **[C]** 0x80002742 takes (address,
value) and sends command 0xE1 (a per-sector dynamic-protection write in the S25FL-S
command convention), not a "flash status 0" write. Init calls it with (0,0) and
(0x40000,0) at 0x80000a74/0x80000a7e; the bootstrap-upgrade path calls (0x40000,0xFF) at
0x8000206a just before erasing that sector. MAIN OS uses commands 0xE2/0xE3/0xE4 on the
record sectors (0x400ca4d6, 0x401375e2, 0x40137704), which are persistent-protection
commands in the same convention (inference **[D]**), so the record sectors are probably
protected on hardware. The bootstrap's receive path has no size limit, so an oversize
container would erase the record sectors.

**Size cap for patchkit: container <= 0x300000 = 3,145,728 bytes** (0x80000 + 0x300000 =
0x380000, 12 whole sectors), ideally stock + a few hundred KB. Chip geometry the bootstrap
recognises: S25FL127S-class 512-byte pages / 256 KiB sectors; other vendors 256 B / 64 KiB
(0x800024ec). DSP blob limits: section 7 stored size <= 0x100000 and depacked size
<= 0x100000 (checked at 0x400cd1d4 / 0x400cd20c), about 700 KB free for the SHARC append. **[V]**

### [C] `tools.firmware.build.rebuild` is not a size to flash

It stores every section literals-only: 4,293,296 B for 1.16 (stock 1,484,064), written to
flash up to 0x4c0000, past 0x380000: it would erase the record sectors at 0x380000 and
0x3c0000 (the emulated receive of such an image erases 17 sectors to 0x4c0000). Never flash a `rebuild` output of MAIN OS. `tools/firmware/inplace.py` decodes the
stock stream into tokens, applies the edit and re-encodes only the tokens it touches:
with no edit it reproduces the stock .syx **byte for byte** (so the token encoder, layout
at 0x90, HMAC, checksums and framing are exact); the one-string image is +32 bytes.
The emulated bootstrap depacks these streams (end marker gamma 2^24+2, 0xFF).
Since the spike 0.3 merge, `rebuild` keeps untouched sections verbatim and takes
`pack='aplib'` (see "aPLib packer" below), but its default still stores replaced
sections literals-only: a store-only MAIN OS alone is about 3.7 MB, over the cap.

## Harmless test image (static facts [V]; emulated boot [D])

`uv run python -m tools.patchkit.patched_image` writes `out/patch/dt2-1.16-harmless.syx` (+ .json,
.png; never committed): UNTITLED -> PATCHKIT at 0x4023b85f (7 bytes), the default project
name shown at the top of the main screen of a new project. `check_authenticity` passes,
only section 3 differs, container 1,484,096 B, the emulated bootstrap accepts, writes and
boots it, and a cold ColdFire boot reaches the main screen with PATCHKIT in the title.
## RAM map, an expansion region and a loader hook (Phase 0.3 of the patchkit plan)

Tools: `tools/coldfire/regions.py` (`report`, `confirm`, `loader-test`), `tools/firmware/aplib_check.py`,
the native examples `regions_probe` and `depack_check` (packages/emulator/boot/examples).
`uv run python -m tools.coldfire.regions report` prints everything below for dt2-1.16,
dt2-1.17, dn2-1.11 and dn2-1.12 from `out/sections/`.

### RAM map of MAIN OS (static facts [V]; DMA arena, task stacks, unreferenced gap, reset stack page [D])

DDR is 128 MiB (`0x40000000-0x47ffffff`, finding 08 **[C]**). The clear end
`0x47e28470` is a real address: it is the end of bss. The 8,066,631-iteration
loop (finding 07) is `(0x47e28470 - 0x40312000) / 16`.

| range (DT2 1.16) | what | evidence |
|---|---|---|
| `0x40000400-0x4031ff60` | MAIN OS image, 3,275,616 B | section 3 |
| `0x40312000-0x4031ff60` | `.data` initialisers, copied to SRAM `0x80000000` (two 32 KiB halves, zero padded) by `FUN_4000045c`, then zeroed by the clear | reset code |
| `0x40312000-0x47e28470` | bss, cleared at reset, 123.09 MiB | `FUN_400004b2` |
| `0x44f39c40-0x46f39c40` | the C++ heap: a buddy allocator, 32 MiB, 16-byte blocks, order 21, bitmap `0x44dc8654` (256 KiB), descriptor `0x402b406c` (in `.data`), mutex `0x44e08654`; the allocator is `FUN_4011e674` (thunk `0x4011e8f0`, 4,248 callers; most likely `operator new`) and the free is `FUN_4011e87a` | decompiled bodies; descriptor words `{0, 0x44f39c40, 0x44dc8654, 0x46f39c40, 0x15, 0x200000}` (the first word is 0) |
| task stacks | static arrays in bss, passed to `task_create` `FUN_400012c8(tcb, entry, prio, stack, size, ...)` at 15 call sites, e.g. `0x409636f0` (4 KiB, prio 9), `0x4096775c` (160 KiB, prio 6), `0x4039beac` (16 KiB, prio 2) | call sites |
| `0x44e4a5b8`, `0x44e4a9b8` | the two display buffers (1 KiB each); the pointers live at `0x402b6058/5c` | `panel_diff` operands, ready snapshot |
| `0x47e28470-0x47e30000` | not cleared, unreferenced, 31 KiB | refscan |
| `0x47e30000-0x47e7a340` | DMA arena, reached through the **uncached alias** `+0x08000000` (`0x4fe30000...`): 82 absolute references, e.g. `0x4fe3b000`, `0x4fe57000`, `0x4fe79300`. Not cleared; the code initialises it. | refscan, canary (pages `0x47e3a000-0x47e72000` written) |
| `0x47e7a340-0x47fff000` | **free** (see below), 1.5 MiB (usable expansion region: [finding 100](100-module-limits.md)) | refscan, canary |
| `0x47fff000-0x48000000` | the reset stack: `main()` runs on it (`0x47fffff0` holds the return addresses `0x40196d7a` and `0x4000056e`); one page used | ready snapshot |

DN2 1.11 has the same shape: image `0x40000400-0x4030b980`, bss
`0x402fc000-0x466b74d0`, heap `0x4464abf0-0x4664abf0`, display buffers
`0x44622bc8/fc8`, DMA arena `0x466b8000-0x46701300` (alias `0x4e6c...`), and
**24.8 MiB** free above it up to the stack (usable expansion region: [finding 100](100-module-limits.md)), because DN2 has no sample memory in
RAM. 1.17 and 1.12 have the same layout (1.17: identical clear range and arena;
1.12: bss `0x402fd000-0x466b84d0`, arena `0x466c0000-0x46709300`).

There is no `malloc` arena above the bss end: no code references `_end`, and
the heap is the fixed buddy arena in bss. Nothing else is allocated
dynamically from the top of RAM.

### Expansion region (static no-reference scan [V]; canary runs [D])

`tools/coldfire/regions.py` proposes the gap above the DMA arena: from the highest
absolute reference into the arena plus 64 KiB, to 128 KiB below the stack top.
It needs no change to the reset clear, because it lies above the bss end.

| image | region | size |
|---|---|---|
| dt2-1.16, dt2-1.17 | `0x47e8b000-0x47fe0000` | 1,396,736 B (1.33 MiB) |
| dn2-1.11 | `0x46712000-0x47fe0000` | 26,009,600 B (24.8 MiB) |
| dn2-1.12 | `0x4671a000-0x47fe0000` | 25,976,832 B (24.8 MiB) |

Evidence, three kinds with different blind spots: (a) no absolute reference into
the region in either view (cached `0x47e...` and alias `0x4fe...`) beyond the
arena cluster; the 3 to 48 stray values in range are constants such as the
softfloat `0x47efffff`; (b) it is above the bss end, so the linker map owns
nothing there and the reset clear does not touch it; (c) `regions.py confirm`
fills it with an address-dependent pattern right after the reset clear returns
(pc `0x40000546`), in both views, and runs the ColdFire emulator without the
DSP: boot to ready plus 300M instructions, a navigation script (every page,
mode, arrow, confirm, modifier and select button, encoder turns and pushes,
about 0.8G instructions), a play script (PLAY, all 16 trigs, RECORD, STOP,
about 1.4G), and a 4G-instruction idle soak. All PASS on the four images: zero
changed words, and the final state digest (with the region zeroed) equals that of
a baseline run that filled zeros, so the firmware did not read the region either.
DT2 also passes `boot` and `play` with the +Drive card `out/plusdrive/dt2.img`.
The harness has teeth: the same run flags the heap, the DMA arena and the
display buffers as written.

What the canary does not cover **[O]**: code paths the scripts do not reach
(project save and load, sample recording and deletion, the SD-card write path,
MIDI SysEx dumps, USB, the firmware update path), and real hardware (the
emulator keeps the cached and alias views as separate memory). The stack page
shows one 4 KiB page used; the 128 KiB reserve is a margin, not a measurement
of the worst case.

### DN2 1.11: the arena's top, measured on the paths the canary misses (dn2_firmware, 2026-10-11)

dn2_firmware measured the top of the DN2 1.11 arena on project save and load,
in both views. Its mods keep data below this region's start, at
`0x46708000-0x46712000`. The sources are dn2_firmware's
`docs/mod-memory-regions.md` and `docs/lfo4-state-memory.md`.

- **[C]** The cached view alone shows nothing about the arena, and on the unit
  the split matters. LFO4 kept its state at `0x46700000-0x46702a00`, over the
  top of the eMMC bounce buffer (`0x4e6f1300`, RAM `0x466f1300-0x46701300`). Every
  project saved on such a unit carried LFO state in 196 windows of its
  12,890,116-byte image. Scans and emulator watches over `0x466...` saw
  nothing, because stock names the buffer only as `0x4e6f1300`.
- **[V]** A touch map of stock in the Rust emulator, both views, at 1 MiB
  pages. The run covered the boot and SAVE PROJECT AS three times. It uploaded
  a compressed project (394 LZ4 decompress calls, 396 eMMC writes). It read the
  project back compressed and raw (654 eMMC reads). It walked every page, play,
  record and stop.
- **[V]** In the alias view only `0x4e600000` and `0x4e700000` were touched.
  Below `0x4e700000 + 0x1340` the bytes are not zero, and above it they are. So
  on these paths the arena ends at `0x46701340`, 0x40 above the bounce buffer's
  end. The literal `0x4e701300` names a second object there.
- **[V]** refscan over all nine views of the RAM (the DDR decode repeats every
  0x08000000 up to `0x8c000000`) finds no address in `0x46701340-0x48000000`.
  Of the 25,450 values in that range in the data from `0x401d0400`, 115 sit
  among pointers. Each was read, and none is an address.
- **[D]** One instrument result: project slot 25 read back identical, 0 of
  12,890,116 bytes, across a flash and a wavetable write while playing. The
  build was a Waverider build, with mod data below this region's start.
- **[O]** So on 1.11 the 64 KiB margin is wider than stock's measured use, but
  only on these paths. The sequencer playing in full, USB audio and the update
  path were not run.

On 1.12 the arena moves +0x8000. The same six bounce-buffer sites, with code
shifted +0x2e0, name `0x4e6f9300`, so the buffer covers RAM
`0x466f9300-0x46709300`. The region rule above follows the arena. A 1.11 data
address carried across by the code shift does not: `0x46708000` lands inside
the 1.12 buffer. Data placements need the arena of the image they run on, in
both views.

Not recommended: holes inside bss. The ready snapshot shows 13 MiB at
`0x44000000` and 24 MiB at `0x45700000` as zero, and a 64 KiB canary at three
places in them passes (boot, and boot plus play with the card), but they are
unused sample-pool and table space of this run, not free space; no static
argument separates them from live arrays.

### Loader hook (hook bytes and free registers [V]; loader-test runs [D])

The appended bytes land past the image end (`0x4031ff60` on DT2 1.16), inside
the range the reset clear zeroes, so the loader must copy them out first. The
reset entry runs, before the caches are enabled (CACR is written at `0x4000055a`):

```
0x4000053e  4eba ff1c   jsr FUN_4000045c(pc)   ; .data -> SRAM
0x40000542  4eba ff6e   jsr FUN_400004b2(pc)   ; clear
0x40000546  ...
```

The same bytes sit at the same addresses on all four images. Replacing the
eight bytes with `4eb9 <cave> 4e71` (`jsr cave.l; nop`) and putting a 34-byte
loader in `flash_cave` works: it reads `[u32 bytes][u32 dest][payload]` from the
image end, copies the payload with a `move.l (a0)+,(a1)+` loop, then calls the
two original helpers and returns. d0, d1, a0, a1 are free at that point.
`tools/coldfire/regions.py loader-test IMAGE --kib N` builds exactly this (segment, cave
code and hook, via `tools.firmware.build.rebuild`), cold-boots the patched image and
compares the region with a random payload at ready: 256 KiB and 1 MiB (aPLib)
on DT2 1.16, 1 MiB (store-only) on DT2 1.17, 1 MiB (aPLib) on DN2 1.11 and
512 KiB on DN2 1.12, zero mismatching words each time, and ready is reached. Code placed this way is in DDR before the
caches are enabled and invalidated, so it needs no cache maintenance. A module
that DMAs from its region must use the `+0x08000000` alias. Module bss is not
zeroed by the reset (the region is outside the clear): the loader must do it.

**Staging limit [V].** The bootstrap stages the packed MAIN OS at `0x40400000`
(`pea 0x40400000` at `0x800005c6` and `0x80000636`) and unpacks to `0x40000400`, so an image longer
than `0x3FFC00` = 4,193,280 bytes writes over the staged stream while it is still
being read. That is harmless while the stream is read ahead of the write point:
`tools.firmware.aplibpack.inplace_margin(stream)` returns the smallest slack. For a DT2 1.16
image with 1 MiB of incompressible payload appended it is 2.1 MB with the
packer below and 4.7 MB store-only. It is a hard limit if a section is stored
with a ratio near 1 and the image is much longer. The flash side is in "NOR flash
map (spike 0.2)" above: the container must stay at or under 0x300000 bytes, a
tighter limit than this one.

### aPLib packer (DT2 1.16 size rows [V]; DN2 rows and native depack harness [D])

`tools/firmware/aplibpack.py` is a compressor for the stream `tools/firmware/elz.py` decodes: hash
chains over 4-byte keys, a forward dynamic program over the exact bit costs of
the format (repeat offset taken from the best path), pure Python.
`tools/firmware/aplib_check.py` packs every section of the four firmwares, reads the
result back with `tools/firmware/elz.py` and runs it through the **bootstrap's own
depacker** (`0x80000432`, found by its prologue; **[C]** earlier text said `0x80000436`, its second instruction) on the native ColdFire core
(`depack_check.rs`; it also reproduces the stock DT2 section 8 byte for byte,
so the harness is faithful). All 16 sections pass both. Sizes in bytes (stored
including the 8-byte header; stock from the .syx table):

| image | section | decoded | packed | stock | store-only | seconds |
|---|---|---|---|---|---|---|
| dt2-1.16 | 3 MAIN OS | 3,275,616 | 1,107,873 | 1,146,652 | ~3,685,000 | 27 |
| dt2-1.16 | 7 BLOB | 321,016 | 187,017 | 184,856 | ~361,000 | 1.4 |
| dt2-1.16 | 8 | 159,948 | 106,986 | 103,416 | ~180,000 | 0.3 |
| dt2-1.16 | 2 bootstrap | 30,302 | 16,558 | 16,136 | ~34,000 | 0.1 |
| dn2-1.11 | 3 MAIN OS | 3,192,192 | 1,096,271 | 1,130,528 | ~3,591,000 | 24 |
| dn2-1.11 | 7 BLOB | 836,956 | 615,802 | 602,076 | ~942,000 | 2.8 |

dt2-1.17 and dn2-1.12 are within 0.1% of their predecessors (MAIN 1,108,851 and
1,097,013). The MAIN OS stream is 3.4% under stock, the other sections 1.2% to
4% over. `tools.firmware.build.rebuild(..., pack='aplib')` uses it (the default stays
store-only until reviewed). **[C]** `rebuild` used to decompress and re-store
every section; a section that is not in `replacements` now keeps its stored
bytes from the source verbatim, so only replaced sections change size (a DT2
1.16 rebuild that replaces MAIN OS is about 1.45 MB with `pack='aplib'` instead
of 4.0 MB). Section 2 (the bootstrap) is therefore no longer re-encoded by any
rebuild.
