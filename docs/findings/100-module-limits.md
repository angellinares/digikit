# 100. Limits for content modules on DT2 1.17 and DN2 1.12

What the firmware allows a run-time module, against what our runtime allows today. Static reads of the images. Marks: **[V]** two agents read the same image
bytes, **[D]** read once, **[O]** open. Reproduce a ColdFire line with `FW_IMAGE=<dt2-1.17|dn2-1.12> uv run python -m tools.coldfire.fw disasm ADDR`; SHARC
lines with `tools/sharc_dev/sharc.py` and `out/sharcdb`. Our values are in `devices/digitakt-ii.toml`, `devices/digitone-ii.toml` and `sdk/sdk.toml`.

## 1. Machines and parameters (ColdFire)

| Item | DT2 1.17 | DN2 1.12 | Evidence | Mark |
| --- | --- | --- | --- | --- |
| Stock machine types | 7 | 5 | type bound `moveq 6` at `0x400c89a4`; DN2 `moveq 4` at `0x400c25f2` | [V] |
| Type storage | one byte (Sound `+0xa2`) | one byte (`+0xde`) | load keeps `(u8)(t+1) < imm`: `0x400db44e` `moveq 8`, DN2 `0x400dd3ea` `moveq 6` | [V] |
| Per-type tables sized to the stock count | descriptors `0x4293b960`, MIDI CC/NRPN tables at `0x400dab9e`, `0x400dac80`, UI list `0x40052a2c` | `0x400dc978`, `0x400dca5a`, `0x400dc18e` | each is 7 (DT2) or 5 (DN2) rows; a new type needs a row in each | [V] |
| Parameter table | `0x4020f59c`, 275 rows of `0x3c` | `0x401f83a4`, 321 rows | accessor `cmpi.l #0x113` at `0x400da03c`; DN2 `cmpi.l #0x141` at `0x400c257c`; 53 absolute references to the base, so rows cannot be appended in place | [V] |
| Row layout | `+4` sound index, `+8/+c/+10` min/max/default, `+28` name, `+30` short name, `+34` formatter code pointer | same | `movea.l (0x34,A0,D0),A0; jsr (A0)` at `0x400c897a` | [V] |
| Knobs per page | 8, plus a Track Level field | 8, plus the same | 8 encoder events `0x29..0x30` at `0x4006cfa8`; DN2 `0x40067904` | [V] |
| Pages per machine | 1 (SRC has one descriptor, no page table) | set by data; stock 4, 3, 4, 1, 1; no cap found | builder `0x400ca988`; page index clamped to count-1 at `0x400168c8` | [V] (page LEDs not read: [O]) |
| Machine slots saved and sent to the DSP | 10 (sound index 25..34), shared by all types | 40 (25..64) | save loop `cmpi.l #0x8a` at `0x400dba94` (table `0x40213a50`); DN2 `cmpi.l #0xc8` at `0x400dd80a` (table `0x401fd330`); frame copy `pea 0x14` at `0x4002eb8c`, DN2 `pea 0x52` at `0x40027526` | [V] |
| Sound array | 71 halfwords | 101 | memset `0x8e` at `0x400db412`; DN2 `0xca` at `0x400dd3ae` | [V] |
| P-locks | any saved slot; 80 lanes per pattern | same | load `0x400dc60a`; DN2 `0x400de682` | [V] |
| Stock DT2 knob use | knob C unused by every stock machine; knob G used by SAMPLE, WERP, STRETCH, REPITCH, SLICED | | descriptor stores from `0x401be1be` | [V] |
| DN2 slot 65 | reaches the DSP, not saved | | save maps it to slot 0; load omits it | [V] |

What this means: on DT2 a module machine can use up to 8 knobs on one page (knob D is the sample slot on a SAMPLE clone, knob H is Track Level unless the
level is fixed another way). On DN2 a machine can have several pages and up to 40 saved parameters. A new machine type needs a row in every per-type table.

## 2. Memory

| Item | DT2 1.17 | DN2 1.12 | Evidence | Mark |
| --- | --- | --- | --- | --- |
| ColdFire RAM | 128 MiB | 128 MiB | `uv run python -m tools.coldfire.ddr_geometry`; only SP load `movea.l #$48000000,a7` at `0x400004f2` | [V] |
| Firmware heap (`fw_alloc`) | 32 MiB buddy heap at `0x44f39c40` | at `0x4464bbf0` | heap descriptor `0x402b45ac`; DN2 `0x4029f0dc` | [V] |
| Expansion region (unreferenced RAM) | `0x47e8b000-0x47fe0000`, 1.33 MiB | `0x4671a000-0x47fe0000`, 24.77 MiB | refscan over the cached and alias views: only float constants and data decoded as code | [V] static; computed addresses [O] |
| SHARC L2 | 1 MiB `0x20000000-0x200fffff` | same | hardware reference ch. 8 | [V] |
| Stock L2 loaded end | `0x2001ab7c` | `0x2008823c` | loader blocks; equals `dsp.l2` start in `devices/ports/*.toml` | [V] |
| Free L2 | 914,564 B below `0x200fa000`, plus 8,192 B at `0x200fe000` after boot | 466,372 B, plus the same 8,192 B | only `0x200fa000` and `0x200fdfff` are referenced above the loaded end | [V] |
| `0x200fa000-0x200fe000` | a cache range set by `0x1c00da` (START2/END2, table at DM `0x2403f0`) | same code | likely non-cacheable [O]; keep it out on both devices | [V] |
| L1 free | block 1 `0x2d7564-0x2ec000` (84,636 B); block 2 `0x300000-0x31c000` (112 KiB) | block 1 `0x2dd52c-0x2ec000` (60,116 B); block 2 same | cache config `0x414141` (16 KiB I, DM, PM caches at the block tops); blocks 0 and 3 are full | [V] static |
| SHARC DDR | 512 MiB configured | 512 MiB | DMC config `0x0af00622` | [V] |
| Stock DDR use | loaded to `0x8055e890`; fill block `0x80a00000-0x82a001c4`; sample pool from `0x8422b9c8` | loaded to `0x8052fbe0`; nothing referenced above `0x80530c00` | loader blocks | [V]; free DN2 DDR [O] |
| DT2 sample pool | 400 MiB, 4 KiB pages | none | DSP bound `0x19200` pages at `0x1c403c`; ColdFire cap `0x19000000` at `0x40153af0`, `0x40153afa`, `0x40154aea`, `0x40154762`, `0x4015470a`, `0x401541fa`, `0x40154226` | [V] |

## 3. Time and the DSP link

| Item | Value | Evidence | Mark |
| --- | --- | --- | --- |
| Core clock | 1 GHz (20 MHz x 100 / 2) | CLKIN `0x1312d00` at `0x120230`; clock word `0x06a10464` built at `0x120532` | [D] (the CTL write is not traced) |
| Block | 32 samples, 1500 blocks/s at 48 kHz | trip count 32 at `0x1c74b5`, DN2 `0x1c9d53` | [V] length, [D] rate |
| Cycles per block | 666,667 | from the two lines above | [D] |
| Free cycles per block | not known for 1.17 / 1.12 | not measured on these images | [O] |
| dsp-link offset field | 17 bits of words, so 512 KiB | `FEXT R14 BY 0:17` in `modules/core/adapters/dsp-link/src/dsp/dsp-link.inc` | [V] |
| dsp-link payload per block | 171 words (DT2), 12 words (DN2) | tail words minus 3 | [V] |
| DN2 voices | 16 slots, chosen at note-on, not tied to tracks | frame stride `0x92`; slot chosen per note | [V] stride; no fixed pair for stereo |

## 4. Ours against the firmware

| Our limit | Value | Firmware allows |
| --- | --- | --- |
| Parameters per machine (`param_slots`) | DT2 5 (`[1,2,4,5,6]`), DN2 8 (`[1,2,4,5,6,0,3,7]`) | DT2 8 knobs on one page, knobs 0, 3 and 7 are TUNE, the sample slot and LEV; DN2 40 saved slots over several pages |
| Pages per machine (patchkit) | 1 | DT2 1; DN2 no cap found |
| `param_rows`, `page_rows`, `machine_rows` | 16, 8 (DN2), 8 | table sizes of ours; the firmware has no such cap |
| DSP area | 512 KiB DT2, 320 KiB DN2 | about 900 KiB DT2 (needs a wider offset field), about 450 KiB DN2 |
| L1 for modules | none | 60 to 84 KiB in block 1, 112 KiB in block 2 |
| ColdFire arena | 512 KiB | DT2 about 1.3 MiB less runtime and staging; DN2 about 24 MiB |
| Module file (`stage_bytes`) | 64 KiB | ours only |
| DDR for modules | none | DT2 sample pool; DN2 512 MiB configured, free range [O] |

## 5. Arena holes after boot, and the steps list

Modules are transferred to the card over USB and enabled in the DIGIKIT menu; they load at the next boot. During the boot load nothing in the arena is freed. For each
enabled module the runtime allocates the module block (`loader.rs`, `arena::alloc(total)`), a buffer for its DSP code and a steps list (`dsp.rs`,
`reserve`), first-fit from an empty arena. The frames that carry DSP code do not flow until the sound engine starts (`boot.rs`, before
`loader::EARLY = true`), so no DSP send (core/dsp-link) finishes and no buffer is freed during the loop: the blocks stack from the bottom. The fit
check counts each module's DSP code buffer as kept (`take` subtracts `arena + arena_xfer`), which is the boot peak **[D]** (read in the code).

What remains:

1. After boot the DSP sends finish and their buffers are freed, which leaves holes between the module blocks for the rest of the session. This
   affects only memory a module allocates at run time (`alloc` in the API) and the largest free block shown during the session (the HEALTH page
   shows it as "CPU BIG"). Module loading is not affected: the selection changes only at the next boot. A fix, if needed: allocate the DSP code
   buffer and the steps list from the top of the arena, so the holes close up when they are freed.
2. The steps list (4 bytes per step plus the 16-byte block header) is not in the needs (`dkneeds`, `arena_xfer` covers the DSP code only), so the
   check is short by tens of bytes per module with DSP code. Fix: add it to the needs.

Plan: [module limits](../plans/module-limits.md).
