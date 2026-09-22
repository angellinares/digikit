# Handover 2026-09-22: XSLICE works; next is a SHARC hook point

Branch `machine-ideas-menu`, PR #37 (https://github.com/m-dwyer/digikit/pull/37).
Everything below is committed and pushed except this file. Results are in
`docs/findings/` (01, 02, 05, 06, 07; index in `docs/FINDINGS.md`); this file
holds state, tools and next steps only. The README was rewritten for 1.16 and
has the full patch-and-verify walkthrough.

Targets: Digitakt II 1.16 (device is on 1.16) and Digitone II 1.11. Flashing
a patched OS is Em's decision; nothing has been flashed.

## Where things stand

- **Flashable eighth machine.** `tools/machinebuild.py` builds a patched 1.16
  `.syx` in one command. XSLICE = MANUAL SLICE clone (type 7), descriptor
  field 2 = `0xfa` (CFADE):
  ```
  uv run python tools/machinebuild.py --syx Digitakt_II_OS1.16.syx --profile dt2-1.16 \
    --machine XSLICE:XSL:6:7 --fields 0xf8,0xf9,0xfa,0xfb,0xfc,0xfd,0,0xfe,0x0a \
    --out OUT.syx --sections-dir out/sections/dt2-1.16
  ```
  Section 3 sha256 `cdbd7726...ecb86a`. In the emulator it boots from reset,
  lists in machine select, commits type 7, and shows a working CFADE knob
  (HUD "Crossfade=50", frame `+0xde = 0x00c0`).
- **Caves.** `cave_b` is zeroed at reset (`FUN_400004b2` clears
  `0x40312000..0x47e28470`). Use `profile['flash_cave']`; `tools/cavefind.py`
  finds them. The XSLICE plan uses 952 of 2108 bytes.
- **What a machine is on the DSP.** The SHARC runs one engine for all
  machines. The type word is only a change detector. `FUN_1c24e9` reads SRC
  frame words per track:
  `+0xde` CFADE discarded; `+0xe6` discarded; `+0xe8` gates a branch at
  `0x1c2727`; `+0xea` (slot 6, mirror 33) stored at table `+12` as
  raw/30720; `+0xec` LEV at `+72` as raw/32768. The table is
  `0x2506ec + R8*0xdc` (32 slots), read by `FUN_1c642a`. The CFADE result is
  from one agent's trace, marked [D][O]: it needs a second check.
- **Emulator speed** is settled: no QEMU port. The cost is Python crossings
  (bitmap getPixel/setPixel HLE, then softfloat). Cuts are not implemented.
- **Tests**: `uv run --with pytest python -m pytest tests -q -k "not real"`:
  789 passed.

## Options for making the machine our own (decided with Em)

1. Data only: put parameters the SHARC uses into SLICE's empty slots. Best is
   slot 6 (e.g. SAMPLE's LOOP `0xd0`, max `0x7802`). This is XSLICE v2; it
   was not built yet.
2. Other machines' parameter rows with different ranges (small effect).
3. Own ColdFire trigger logic in the cave for type 7 (random/sequenced slices,
   stutter, velocity-selected slices). This needs the 1.16 note-on /
   slice-feed path; start from the type-4/6 feed at `0x4002e604`. Not started.
4. New SHARC DSP code (voice FX, granular, an oscillator that needs no
   samples). Needs a hook point, Selache to assemble, `sharc_trace` to test,
   and a hardware test. Frame words the stock converter discards (CFADE) are
   still readable by our own DSP code, so they are free parameter channels.

## Next steps (Em's direction: SHARC hook point first, all static, in parallel)

1. **DT2 audio root → hook candidates.** From the SHARC+ hardware reference in
   `out/refs/` (interrupt vector table layout, SPORT DMA interrupts), find the
   IVT entries in the image, the audio ISR, and the call chain to the
   per-block render callback and the per-voice code. Resolve the indirect jump
   in `FUN_1c642a` at `0x1c6579` (`I12`/`M13`; `sharc_trace` already runs 149
   instructions of it with `R8=0x2506ec, R4=0x2412c8`). Output: ranked hook
   sites with register and calling context. Use the Ghidra dump
   `out/ghidra/sharc-dt2-1.16/` (`calls`, `data_refs` in `xrefs.sqlite`).
2. **Digitone II SHARC.** Build its Ghidra dump (one JVM; the import may need
   `tools/sharc_import.py` setup like DT2's `sharc-dm-dt2-116` project), find
   its engine selection (it has several real synth engines), and match
   functions shared with DT2 (RTOS kernel, frame reader) to carry names across.
3. **Offline renderer.** A harness that runs a SHARC routine in
   `tools/sharc_trace.py` on a buffer and writes a WAV. Prove it on a stock
   routine first, then use it to hear new code without hardware.
4. Then, if wanted: XSLICE v2 (drop CFADE or keep it as a DSP parameter
   channel, add LOOP `0xd0` at slot 6), checked with one `machinecheck` run
   against a real SLICE baseline. And the second check of the CFADE-discarded
   finding.

## Tools added this session

| tool | use |
|---|---|
| `tools/machinebuild.py` | stock `.syx` → patched `.syx`; audits for 1.15C literals |
| `tools/machinecheck.py` | select/turn/read-frame/PNG run, baseline in parallel |
| `tools/cavefind.py` | caves that survive boot; `--confirm SYX` canary boot |
| `tools/speedab.py`, `tools/qemuceiling/` | timing A/B, crossings, profile; QEMU vs Unicorn ceiling |
| `emu/semscan.py` | never-fake semaphores from the image |
| `tools/selasm.py` | Selache assemble + parcel swap + side-by-side decode |
| `tools/sharcemu.py` | now has a JVM-free pypcode backend (most ALU forms still lack semantics) |
| `tools/sharc_trace.py` | concrete SHARC execution; FEXT se, register ASHIFT, value logging added |

## Gotchas

- **Do static checks before any emulator run.** Em objected to long runs.
  The rank-shim hang took several boot rounds but was a 1.15C address that
  the audit now finds in milliseconds. Batch what's left into one bounded run.
- A patched `.syx` needs a **fresh boot**: the snapshot manifest records the
  flash sha. `guirun --input` rejects a patched `.syx` (`identify()`); use
  `--feed` or `machinecheck`.
- After resuming the 400M rung, the "FACTORY PROJECT >> +DRIVE" dialog clears
  about 345-360M instructions later; send inputs after that.
- Machine select: the first YES commits and the second closes. SLICE is an
  interior row (list clamps only at the end), so use exact DOWN counts.
- The harness never raises vector 191; frame reads need the gate
  `0x409664f4` opened and 191 raised (`machinecheck` does this; its capture
  does not return cleanly yet, so the type word reads 0).
- `emu.dspboot.run` (cold boot) and `emu.longrun.build` (resume) count tasks
  differently; don't compare them.
- The SHARC Ghidra dump's addresses are **2x short-word addresses**.
  `tools/sharcfn.py` prints the wrong MODIFY destination (real: `is XOR idis`).
- Selache reads VISA parcels big-endian and the image is little-endian per
  parcel: swap before comparing. It leaves relocations unresolved.
- Subagent drafts in this job's tmp dir (`$CLAUDE_JOB_DIR/tmp/`) go away with
  the job; everything needed is committed or reproducible with the commands
  above.
- A coder once overwrote an existing test file when copying a draft; check
  `git diff --stat` after file copies.
