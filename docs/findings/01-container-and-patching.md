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
  reason `tools/patchimg.py` refuses section 2 outright.
- **The Unicorn depacker cannot read them.** Every packed section of 1.16
  fails under `emu.extract --oracle`. The cause is the oracle, not the new
  section: the depacker is taken from the UPDATER, and 1.16's UPDATER differs
  from 1.15C's by **43.4%** (14,231 of 32,776 bytes, first difference at
  offset `0x9b`), so the entry point at `0x80000432` has moved. **[V]**
  `dt2/elz.py`, a byte-level decoder that `emu.extract` now uses by default,
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
garbage. Use Ghidra's `68000:BE:32:Coldfire` or `dt2/coldfire.py`.

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
from the two `.syx` files with `dt2/container.py`, and the floor is `"006/"`, so
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
is a strong inference from the code layout, not demonstrated. **[O]**

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

`dt2/build.py` rebuilds 1.16 after two changes: packed sections are
decompressed with `dt2.elz.depack_section` (the oracle depacker's entry
point moved on 1.16), and section 8 is stored like 2/3/7. A rebuild with no
replacements re-extracts to six sections byte-identical to the stock
extraction, and `tools/roundtrip.py`'s `check_authenticity()` passes
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

`tools/cavefind.py` finds caves that survive boot: it reads the copy and
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

