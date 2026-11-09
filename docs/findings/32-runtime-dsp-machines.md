# 32. A machine with its own DSP code, loaded at run time (digikit runtime, Phase 5, DSP side)

Plan: `docs/plans/digikit-runtime.md` (Phase 5). Design: `docs/design/digikit-dkm.md` ("DSP section"), `docs/design/digikit-runtime.md` ("DSP code of a
module"). Code: `modules/core/runtime/src/rs/src/dsp.rs`, `loader.rs`; `modules/core/adapters/dsp-link` (windows); `modules/core/adapters/dyndsp` (new);
`packages/patchkit/src/dkmdsp.rs`; `tests/fixtures/modules/dyn-pluck`, `tests/fixtures/modules/dyn-sine-dn2`; proof `tools/patchkit/pk_dkrt_machine.py`.
Lane r4, 2026-10-17, one agent, native emulator. Everything is **[D]** (measured here once; no second agent has checked it against the image
bytes). Nothing ran on a device.

## Result

**[D]** `digikit/pluck` loaded at run time (`test/dyn-pluck`, a `.dkm` of 4 484 bytes: the typed machine and four parameter rows and the pluck
DSP routine) plays **bit-identical** to the patched-in `digikit/pluck` on Digitakt II 1.17 and 1.16: a 1 s note (96 000 samples captured, both
channels) from the same DSP block, 0 samples differ from the note's onset on. Proof: `uv run python -m tools.patchkit.pk_dkrt_machine proof
--image dt2-1.17` (about 65 s with a warm daemon; `--quick` skips the control note and the reload; `mise run dkrt:machine`).

| Step (DT2 1.17) | Result |
| --- | --- |
| two images with the same ColdFire modules (`core/runtime`, `core/dynreg`, `core/dyndsp` and what they require): reference with `digikit/pluck` patched in, run-time image without | same hooks in every block interrupt, so the note-on lands in the same block |
| `dkrt_load("modules/pluck-d.dkm")` while the firmware runs | answers `LOADING` (1); the final result is 0 after 285 DSP blocks (190 ms), most of it UI-loop passes (about 7 passes at roughly 37 Hz) |
| the machine row (type 18) while the DSP code is on its way | not listed (sampled every 2 ms of the load); listed after |
| DSP area (`_dl_area`, 512 KiB) after the load | the 3 532 bytes at the placement are the image relocated by a second implementation (`pk_dkrt_machine.relocate`), 0 bytes differ; the 256 KiB `.PKBSS` delay-line buffer is reserved behind it, not sent |
| voice rows | exactly one claimed: `{type 18, pc 0xb8d740}`, the routine's pc at the placement; the other seven stay `0xffffffff` |
| PCM, same trig half a block after the same DSP block, run-time against patched-in | **0 samples differ** from the onset on (onset at the same sample in both; the run-time capture is 64 samples longer, so the common part is compared) |
| control: after the unload (the track keeps type 18, the row is free again) the same note | differs (the DSP plays the stock SAMPLE renderer, peak 0.036 against 0.128), so the comparison can fail |
| unload, then reload | the machine row goes first, then the voice row (type back to `0xffffffff`), then the range; the reload reuses the same range |
| write-watch on `cf.expansion` (the staging buffer excluded: the card read lands there by DMA) | 0 events in both runs |
| flush check | 0 hits; 706 CPUSHL in the run-time image (the module's code and the relocated records are pushed) |
| card store sectors written | 0 |

DT2 1.16 gives the same numbers (load 282 blocks, onset shift 0, 0 differ). The Digitone II half is below.

**How the note is aligned.** Two images with different ColdFire code do not reach the audition at the same instruction, and a key that arrives
close to a block boundary may start the note one DSP block (64 interleaved samples) earlier or later: without care the first runs differed
by exactly one block and, on 1.16, by a capture phase of 32 samples, with 0 differing samples after aligning the onsets. The proof
therefore presses and releases the key half a block (44 000 ColdFire instructions) after a fixed DSP block (`run_until frames`), which
puts both images' key paths far from a boundary; the onsets then coincide on all four images, and the check is sample for sample from the
onset on (it also accepts an even onset shift of up to 128 samples with 0 differing samples after aligning, and reports it).

## How the DSP code is placed and sent

**Placement: relocation on the ColdFire, not fixed slots.** The repository's DSP objects use only two relocation kinds (`sharc.abs24`: a call or
jump pc; `sharc.abs32`: a 32-bit immediate, a data byte address or a pc): `pluck` has 24 relocations in 3 532 bytes, the sine voice 5. Text symbols
are short-word pcs, data and bss symbols byte addresses, so a placement at the L2 byte address `B` shifts pcs by `(B - 0x20000000) / 2` and data
addresses by `B - 0x20000000`. `patchkit pack --device` links the object at the nominal base `0x20000000` and emits one entry per relocation
(offset, kind, class text/data/import); the runtime adds the shifts on the ColdFire into an arena copy before it sends, so several DSP modules
coexist and a range is freed on unload. This was cheap (about 200 lines of Rust for the packer and its reference loader, 100 of the runtime's);
fixed per-type slots were not needed. The format is in `docs/design/digikit-dkm.md`.

**The area had to grow.** `pluck` reserves 256 KiB of delay lines (`.PKBSS`, 16 tracks of 4096 samples), which does not fit the 128 KiB `_dl_area` of
finding 31. The area is now 512 KiB on the Digitakt II (DSP L2 free: 914,564 B below 0x200fa000, 525 836 used with the area, runtime and
`core/dyndsp`) and 320 KiB on the Digitone II (466,372 B below 0x200fa000, [finding 100](100-module-limits.md); `pluck_dn2` needs 262 144 + 3.6 KB). That needed a wider
offset field in the chunk header: **[C]** the wire format of finding 31 changed. w0: magic 16 bits, `idx` 14 bits, `win` 2 bits; w1: `off` 17 bits (words),
`n` 8 bits, LAST, END, `xid` 5 bits. The sender (`dl_send`) is unchanged for callers; `dl_send_win(win, dst_word, buf, len)` is new. Chunk counts
stay under the 14-bit `idx` (512 KiB at the DN2 48-byte payload is 10 923 chunks). The DSP stub checks `n`, then the window: window 0 is the
area (limit `DL_AREA_WORDS` of the device file), window 1 the voices registry (`2 * voices.count` words, base `voices.rows`; both are 0 when
`core/dsp-voice` is not in the image, which refuses every write). `tests/patchkit/test_dsplink.py` (19 tests) and `pk_dsplink proof --quick` on DT2
1.17 pass with the new format (4 096 B byte exact, 11 blocks, PCM bit-identical to the idle image).

**Voice rows.** `core/dyndsp` adds eight rows `{type 0xffffffff, pc 0}` to the `voices` registry of `core/dsp-voice` (rows with a non-zero type
the scan compares and skips; no machine type equals `0xffffffff`). The runtime claims a row for a voice, writes the pc (one word, window 1,
a transfer of its own), then the type; it frees a row by writing the type first. A word store is atomic for the render scan, and the type is
the word that makes the row valid, so the scan never sees a half-written row. Images with `core/dsp-voice` and without `core/dyndsp` are
unchanged. **[D]** The byte order matters: `dl_send` sends a buffer as little-endian DSP words, a ColdFire long is big endian, so the one-word
write swaps by hand (`swap_bytes` compiles to a libgcc call the runtime does not have: the first attempt jumped to address 0 and
faulted).

**The load is a state machine.** `dkrt_load` returns `LOADING` after the synchronous checks and the reservation; `dsp::step`, called from `dkrt_tick`
in the UI-loop hook, runs `PH_SEND` (image), `PH_ROWS` (two one-word transfers per voice), then the shared tail (machine rows, `init`, slots). The
UI task is never blocked by the 70 ms (DT2, 64 KiB) to 1 s (DN2) transfer. More requests queue (four); unload while loading answers `E_BUSY`; a
failure undoes the DSP part and the ColdFire part. `dkrt_status` and `dkrt_module` report the state. Refusal codes -30 to -37 (`dkrt-sdk`).

## What the emulator needed **[D]**

The SHARC+ engine's instruction fetch for an L2 pc (`Mem::read_sw`) looked at the loader's bytes and their short-word alias, but a DM store to a byte
that no boot block loaded goes to the alias page `address + 0x28000000` (`canonical_of`), which the fetch did not look at. Code the DSP wrote at run time
into `_dl_area` (a `.PKBSS`: not in the boot stream) therefore halted the DSP with "no decoded instruction at 0xb8d740" the first time a track used
it, while `dsp.peek` of the same bytes read them fine. `packages/emulator/sharc-rt/src/mem.rs` (`read_sw`, `watch_sw`): one more candidate address.
**Outside the lane's files**; on a device the L2 is one memory and nothing like this exists. The new code runs in the interpreter (patched code
always does here), which is why a pluck note costs wall time.

## Digitone II

**[D]** The ColdFire registry of DN2 run-time machines (pages, `own_dsp`) is lane B's, so only the DSP half is proven: `test/dyn-sine-dn2` (the DSP
source of `test/dn2-sine`, no machine record, a `[[dsp_voice]] type = 18` entry) is packed with `patchkit pack --device dn2`, loaded
at run time through the mailbox and rendered, with the DSP's frame copy forced to the machine type (`DSP_FRAME_POKE=0x94=0x12`; the
reference is `test/dn2-sine` patched in with `0x94=0x7`; both images have `core/runtime`, `core/dyndsp`, `core/machines`). DN2 1.12 and 1.11:

| Step | Result |
| --- | --- |
| `dkrt_load` | `LOADING`, then 0 after 153 blocks (the 384-byte image is 8 chunks of 48 bytes) |
| DSP area (320 KiB at `0x20088540`) | the relocated image byte for byte, 0 differ; one voice row `{18, 0xbc42a0}` |
| PCM, TRIG 1 held 200 ms | the same peak (0.0275) and **0 differing samples over the first 9 000 frames after the onset** (the onset is the same sample) |
| the key release | arrives a block apart in the two images (ColdFire code on the key path differs: `test/dn2-sine` adds a machine module), so the samples after the release (from frame 9 733) differ by less than 1e-6 while the voice is the same; not compared |
| unload, reload | the row is free again (type `0xffffffff`), the range is free, the reload takes the same range |
| write-watch, flush check, card store | 0 events, 0 hits (258 CPUSHL), 0 sectors |

The DN2 chunk rate (48 bytes per block) limits a big module: the 3.5 KB `pluck_dn2` image is 76 chunks (about 0.1 s), a 64 KiB file about 1 s, as
finding 31 measured. `pluck_dn2` itself (262 144 bytes of delay lines) fits the 320 KiB area; it was not run at run time (its machine page is lane B's).

## Open **[O]**

- **Selector word of a run-time type.** Not done. The DSP's type to selector table is `mf_selector` of `core/machines` (`dsp.l2`, one word per type
  0..23 on the Digitakt II, copied from the stock table and extended with the clone's word). A third window of `core/dsp-link` onto `mf_selector`
  (24 words, DT2 only), a `selector` field in the DSP section's voice record, one more one-word transfer before the type word, and the reverse on
  unload would let a run-time SLICE clone play as SLICE (the open point of finding 30); a PCM comparison against the patched-in `test/xslice`
  would prove it. Estimated 3 to 4 agent hours; `pluck` clones SAMPLE, whose selector (0) the reserved rows already have.
- **Hardware**: the cache sequence of the DSP stub (finding 31) now also covers code the runtime placed at run time; the emulator models no caches.
  The relocated image is read in place by the frame hook (interrupt) while the UI task owns the buffer: it is not freed before DONE.
- **Blocking unload**: clearing a voice row spins in the UI task for about ten audio blocks (bounded by a spin limit). A hardware menu would
  rather see an `UNLOADING` state; the order (machine row, voice row, range) is what matters.
- **Menu**: lane C owns it; the only change here is that `LOADING` counts as accepted. The answer row does not show a late failure of an
  asynchronous load (`last_result` does).
- **Digitone II machine rows** (pages, `own_dsp`): lane B.
- **More than 8 voice rows or 8 modules**, DSP code over 64 KiB (the staging buffer), DSP data that must be initialised to something other than
  the image (the area is uninitialised L2 on a device, finding 06; `.PKBSS` is never zeroed by the runtime, and `pluck` writes its lines before it reads them).
