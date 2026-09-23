# Digitakt II firmware research

An emulator for the Elektron Digitakt II's control processor, plus tooling for
understanding and (eventually) patching its OS images. The same tooling also
covers the Digitone II, which shares the ColdFire architecture and most of the
container/machine machinery.

**No Elektron firmware is included in this repository and none ever should
be** — it is copyright Elektron. You supply your own lawfully-obtained `.syx`;
`.gitignore` keeps it, the extracted sections and the snapshots out of git.

## Current firmware

Targets are **Digitakt II OS 1.16** and **Digitone II OS 1.11**. Digitakt II
1.15C and Digitone II 1.10E are kept as history — most of the address-specific
analysis in `docs/findings/` was originally done against them, and later
sections mark what changed on the newer builds — but they are not what new
work should target. Em's own device was upgraded to 1.16 on 2026-09-20.

Every tool that resolves a firmware by name or by SHA-256 knows about all
four:

| device | version | filename | sha256 |
| --- | --- | --- | --- |
| Digitakt II | 1.15C | `Digitakt_II_OS1.15C.syx` | `62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c` |
| Digitakt II | 1.16 | `Digitakt_II_OS1.16.syx` | `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec` |
| Digitone II | 1.10E | `Digitone_II_OS1.10E.syx` | `9881ee5ca4624be2b833e5eb94e294f2f8425371e6729781baccd1c256cac504` |
| Digitone II | 1.11 | `Digitone_II_OS1.11.syx` | `2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0` |

(`devices/digitakt-ii.toml`, `devices/digitone-ii.toml`.)

Two things to know before running anything:

- **`sections/` holds exactly one firmware at a time.** The extracted section
  filenames are fixed, so `sections/.source-sha256` records which `.syx` they
  came from; every tool that reads sections checks it and refuses to pair
  them with a different firmware. Switching firmware re-extracts. To keep two
  firmwares' sections around at once, point `DT2_SECTIONS` (and, for
  snapshots, `DT2_SNAPSHOTS`) at separate directories — the convention used
  throughout this repo and in `docs/findings/` is
  `out/sections/dt2-1.16`, `out/sections/dt2-1.15C`, `out/sections/dn2-1.11`,
  with matching `snapshots/dt2-1.16/` etc.
- **`emu/config.py` still breaks a multi-`.syx` tie in favour of 1.15C** (its
  `TESTED_SYX`), because that is the build most existing snapshots and cached
  sections were made against. It never overrides an explicit argument or
  `DT2_SYX`, so naming `Digitakt_II_OS1.16.syx` explicitly (as the quick start
  below does) always gets you 1.16 regardless of what else is in the working
  directory.

## What it does

It boots. On both 1.16 and 1.15C (and 1.11/1.10E on Digitone II) the main OS
runs, spawns its RTOS tasks, reaches its message loop and **renders its user
interface** — pattern and project name, tempo, the encoder parameter row, the
sample page with its knob widgets — into a 128x64 panel you can watch live, or
dump to a PNG with `emu.panel`. `tools/emucheck.py` gives a deterministic
pass/fail for "did it really get there" on any of the four builds.

It is a research instrument, not a Digitakt you can play, but it is no longer
too slow to watch. Block-bounded stepping reaches roughly 10M instructions a
second — ahead of the 4.68M the timer models pace the firmware's own clock to,
though still far short of the real part's ~200-264M — so the panel renders at
20–25 fps and the GUI spends the surplus sleeping, keeping the emulated clock
and the wall clock together. See **What works, and what does not** below.

## Quick start

You need Python 3.12 (via [uv](https://docs.astral.sh/uv/)) and your own
firmware file in the working directory.

```sh
uv sync
tools/install-patched-unicorn.sh

# Digitakt II 1.16
uv run python -m emu.run Digitakt_II_OS1.16.syx

# Digitone II 1.11
uv run python -m emu.run Digitone_II_OS1.11.syx
```

That checks each prerequisite, extracts the sections and builds the boot
snapshots on first run (one cold boot from reset, a few minutes — it happens
once), and opens the live panel. Because 1.16/1.11 are not `emu/config.py`'s
historic "tested" build, `emu.run` keeps their sections and snapshot ladder in
their own subdirectory (`snapshots/Digitakt_II_OS1.16/…`) rather than the
historic `sections/`/`snapshots/` paths, and prints a one-line note rather
than an error.

**No flags are needed.** Three you will see in older notes are not:

- `--slc` forces the eMMC SLC flag, which the eSDHC storage model already
  supplies. `build()` turns that model on by default.
- `--unthrottled` used to be how you got a watchable frame rate. It is now the
  wrong choice: the emulator outruns the firmware's clock, so without the flag
  the GUI paces itself to real time and the sequencer keeps proper tempo, and
  with it everything runs about 1.5x too fast.
- `--weakptr` steps over the weak_ptr branches that used to freeze the main
  task. It is a fallback for a boot that stalls, not a default.

`--exact` is worth knowing about: it swaps block-bounded stepping for exact
`count=` stepping, about 5-7.6x slower, and is what to use when comparing a
run against `tools/bootcheck.py` or `tools/speedab.py`.

Unicorn needs the repo's patches (SR read and CCR sync, EMAC MAC with load);
see [docs/UNICORN.md](docs/UNICORN.md). `uv sync` can restore stock Unicorn,
which the emulator deliberately rejects until the installer is rerun.

### Extracting and building the ladder by hand

`emu.run` does the following for you; do it directly when a tool wants
explicit paths, or for headless/CI use:

```sh
# 1. Decompress the sections. dt2/elz.py, a byte-level decoder written from
#    the format in elektron-firmware-tool's aplib.c, is the default and reads
#    all four firmwares in the repo root in about a second.
uv run python -m emu.extract Digitakt_II_OS1.16.syx -o out/sections/dt2-1.16

# --oracle instead runs the DEVICE'S OWN depacker under Unicorn, as a
# cross-check. It only works on Digitakt II 1.15C and Digitone II 1.10E: the
# depacker's entry point in section 4 (the updater) moves on 1.16/1.11, and
# section 4 differs from 1.15C's by 43.4% of its bytes.

# 2. Build the boot snapshot ladder (about 6 minutes for 1.16).
DT2_SECTIONS=out/sections/dt2-1.16 DT2_SNAPSHOTS=out/snapshots/dt2-1.16 \
  uv run python -m emu.checkpoint make \
  60000000,120000000,200000000,280000000,400000000 \
  out/snapshots/dt2-1.16/boot Digitakt_II_OS1.16.syx

# 3. Check it actually reached a trustworthy main screen, not just a
#    plausible-looking frame (panel density, +Drive formatted, no unblock
#    faked against a semaphore with a real task-code poster).
uv run python tools/emucheck.py --device dt2 --syx Digitakt_II_OS1.16.syx \
    --sections out/sections/dt2-1.16 \
    --snapshot out/snapshots/dt2-1.16/boot400M.snap --instrs 600000000

# 4. Watch it. emu.gui reads the MAIN OS image through DT2_SECTIONS too.
DT2_SECTIONS=out/sections/dt2-1.16 \
  uv run python -m emu.gui out/snapshots/dt2-1.16/boot400M.snap \
    --syx Digitakt_II_OS1.16.syx
```

The same four steps work for Digitone II with `--device dn2`,
`Digitone_II_OS1.11.syx` and an `out/sections|snapshots/dn2-1.11` prefix.

### Options

```sh
uv run python -m emu.run [firmware.syx] [snapshot] [options]
```

| | |
| --- | --- |
| `--weakptr` | Step over two branches in `weak_ptr::lock` that otherwise freeze the main task after 153 messages. They contradict the memory they branch on — an emulator defect, not a firmware decision. Recommended. |
| `--slc` | Force the eMMC "SLC mode" flag. Unnecessary now that the eSDHC model supplies it. |
| `--scale N` | Integer panel zoom. Defaults to whatever fits your screen. |
| `--check` | Resolve and validate everything, then stop without running. |
| `--accept-sections` | Confirm the extracted sections really are the firmware you named. |
| `--exact` | Exact `count=` stepping instead of block-bounded stepping (5-7.6x slower); use it when comparing against `bootcheck.py`. |
| `--unthrottled` | Run as fast as the host allows, instead of pacing to the hardware's own clock. |

Nothing is hardcoded to a filename. Paths resolve from an explicit argument,
then an environment variable, then discovery:

| | |
| --- | --- |
| `DT2_SYX` | the firmware `.syx` |
| `DT2_SECTIONS` | directory of extracted sections (default `sections`) |
| `DT2_SNAPSHOTS` | directory of boot snapshots (default `snapshots`) |
| `DT2_MAIN_IMG` | the decompressed MAIN OS image |

## What works, and what does not

**Working:** the ColdFire core, the interrupt controllers, the PIT and DMA
timers, eDMA, the front-panel link, the coprocessor port's handshake, the
display, and the SD/MMC controller through card identification, on all four
firmware builds.

**Storage is not backed with real data.** The eSDHC controller and an eMMC
are modelled far enough to complete card identification, read EXT_CSD and run
the firmware's own "factory reset" formatting worker (2,120 erase groups plus
a header sector), so a first boot on a blank card reaches the main screen
instead of stalling in "INITIALIZING +DRIVE...". But no sample or project data
is served — the emulated +Drive always comes up freshly formatted and empty,
with whatever blank default project the firmware itself creates on an empty
card, not one you made — so anything that reads back real sample or saved
project content will not work. Backing it with a real image, and then
generating one from a folder of samples, is the next substantial piece of
work.

**No audio.** The device makes sound on a second processor — an Analog Devices
ADSP-21569 SHARC+ running its own firmware. Nothing here emulates it; see
**The SHARC side** below for what static tooling can do instead. The main OS
holds parameter state and RPCs it to the SHARC over a periodic DSPI2 frame;
that split is what makes audio a separate project rather than a missing
feature.

**No input in the emulator's own worker loop**, beyond what the GUI's panel
clicks and `tools/guirun.py --input`/`--feed` inject: the front-panel protocol
is decoded in the transmit direction and the receive side is modelled well
enough to drive machine selection and encoders under test tooling, but there
is no live human-input device.

## Building a patched OS with a new machine

### What a machine is

On the ColdFire side, a machine is not a block of arbitrary code: it is a
44-byte descriptor of nine parameter-code fields, one per SRC-page slot, that
the UI and the parameter tables read generically — the descriptor chooses
*what each fixed engine slot means* (name, range, default, formatter), not
where it lives (`docs/findings/02-machines-and-parameters.md`, "A machine is
eight fixed engine slots"). Slot 2 (mirror index 27, CFADE) is wired into every
descriptor and sent to the DSP, but no stock machine exposes it, and the
SHARC's parameter converter discards it (findings 06, "Where the SRC-page
words go"). Slot 6 (mirror 33) is the unused slot the SHARC does store. On
the SHARC side, the same slots arrive in the periodic TX frame and are read
unconditionally regardless of machine type; the machine-type word the SHARC
receives is only a change detector, not a dispatch key, so a new ColdFire-side
machine type does not by itself change what the SHARC does with a slot's value
(`docs/findings/06-sharc-engine-and-startup.md`, "`FUN_1c24e9` reads the SRC
page"). A cloned machine with a new descriptor is therefore cheap; making the
SHARC treat a slot differently is not — see **The SHARC side**.

### Building one

`tools/machinebuild.py` builds a flashable `.syx` with an eighth machine
baked into MAIN OS. It wraps `tools/machinepatch.py`'s nine-part patch (list,
dispatch, group, name, rank, permit, hint, pertype, clone — see
`docs/findings/02-machines-and-parameters.md`), captures the new machine's
descriptor fields live (the descriptor array is bss, so it cannot be read
from the static image), plans all nine parts into the profile's `flash_cave`
— free space proven to survive a cold boot, as opposed to `cave_b`, which the
reset path zeroes before the OS runs — verifies every write against the image,
audits the result for any address that belongs to a *different* firmware
(a sign the wrong profile was used), rebuilds the `.syx`, and re-extracts it
to confirm section 3 matches and every other section is untouched.

This is exactly how XSLICE — a flashed eighth machine (type 7, a MANUAL SLICE
clone) with a working CFADE control on its SRC page — was built and checked
(`docs/findings/02-machines-and-parameters.md`, "XSLICE"):

```sh
uv run python tools/machinebuild.py \
    --syx Digitakt_II_OS1.16.syx \
    --profile dt2-1.16 \
    --machine XSLICE:XSL:6:7 \
    --fields 0xf8,0xf9,0xfa,0xfb,0xfc,0xfd,0,0xfe,0x0a \
    --sections-dir out/sections/dt2-1.16 \
    --out out/Digitakt_II_OS1.16.xslice.syx
```

`--profile` is one of `dt2-1.15C`, `dt2-1.16`, `dn2-1.11` (`tools/machineprofile.py`'s
per-image address tables). `--machine NAME:SHORT[:CLONE_OF[:POSITION]]` names
the new machine and which stock type's descriptor fields it starts from;
`--fields` overrides those nine fields directly (here, MANUAL SLICE's own
fields with field 2 set to `0xfa`, MANUAL SLICE's orphaned CFADE parameter).
Omit `--fields` and pass `--fields-from SNAPSHOT` to capture a stock machine's
fields live from a resumed boot snapshot instead. Run `--help` for the rest
(`--parts` to patch only a subset, `--clone-of`, `--extract-dir`).

Finding free space for this yourself, or for a different change, is
`tools/cavefind.py`: it locates runs of filler bytes in a MAIN OS image that
are unreferenced by both a static immediate scan and Ghidra's reference
tables, and confirms with a canary-filled boot that the reset path really
leaves them alone:

```sh
uv run python tools/cavefind.py out/sections/dt2-1.16/section_3_MAIN_OS.bin
uv run python tools/cavefind.py out/sections/dt2-1.16/section_3_MAIN_OS.bin \
    --confirm Digitakt_II_OS1.16.syx
```

### Verifying in the emulator

```sh
# Extract and boot the patched image to one rung.
uv run python -m emu.extract out/Digitakt_II_OS1.16.xslice.syx \
    -o out/sections/dt2-1.16.xslice
DT2_SECTIONS=out/sections/dt2-1.16.xslice \
  uv run python -m emu.checkpoint make 400000000 \
  out/snapshots/dt2-1.16.xslice/boot out/Digitakt_II_OS1.16.xslice.syx

# Select the new machine on a track, turn its encoder, read the TX frame,
# and compare against a stock baseline running in parallel. --frame-* override
# the stock profile's addresses: framelink.py keys by MAIN OS's own SHA-256,
# and a patch changes it, so a patched build needs these spelled out (none of
# machinepatch's nine parts touch them, so the stock values are still right).
uv run python tools/machinecheck.py \
    --syx out/Digitakt_II_OS1.16.xslice.syx \
    --sections out/sections/dt2-1.16.xslice \
    --snapshot out/snapshots/dt2-1.16.xslice/boot400M.snap \
    --select-type 7 \
    --turn 2:30 --turn 2:30 \
    --frame-words 0x94,0xde,0xe6,0xea \
    --frame-gate 0x409664f4 --frame-vector 191 \
    --frame-handler 0x4002dd0c --frame-driver 0x400cd2bc \
    --frame-counter 0x402a1488 \
    --png-dir out/machinecheck/xslice --json out/machinecheck/xslice.json \
    --baseline-snapshot out/snapshots/dt2-1.16/boot400M.snap \
    --baseline-syx Digitakt_II_OS1.16.syx \
    --baseline-sections out/sections/dt2-1.16 \
    --baseline-select-type 6
```

`tools/machinecheck.py` presses YES twice (a changed selection commits but
does not close the list; a second YES on the now-current row closes it),
opens the TX-frame-build gate and raises vector 191 by hand (nothing in the
emulator's timer model does this on its own), and reads back the chosen
frame words. `--select-type` and `--baseline-select-type` take the raw
machine type number (7 for a first cloned machine, 6 for stock MANUAL
SLICE/SLICE). Each `--turn` message is clamped to ±30 by the firmware, so
reaching a larger cumulative value takes several.

## Hardware warning

**No patched image built by this repository has ever been flashed to a real
device.** Flashing one is entirely Em's decision, made deliberately and
separately from writing or checking any tool here.

- **Never modify sections 2 (bootstrap) or 4 (updater).** `tools/patchimg.py`
  hard-refuses any patch targeting them. Everything this repo builds — the
  machine work included — only ever touches section 3 (MAIN OS).
- **The bootstrap version gate is one-way and hard to trigger by accident.**
  At `0x80001c72`, `bcc` (unsigned "greater than or equal") exits without
  upgrading when the running bootstrap version is already `>=` the incoming
  one; the one-way **BOOTSTRAP UPGRADE** operation runs only when the
  incoming version is *strictly greater*. The bootstrap version word is
  `0x0200` on 1.15C/1.10E and `0x0201` on 1.16/1.11. A patch that only
  touches section 3, as everything above does, never presents section 2 at
  all, so this gate never comes into play for that kind of patch — it matters
  only if you are ever tempted to install a whole newer-generation `.syx`
  over an older bootstrap.
- **MAIN OS's own upgrade check is a fixed build-number floor, not a version
  comparison** — `"006/"` on 1.16, meaning "build >= 0060" — so a same-version
  rebuild is not rejected as a downgrade (`docs/findings/01-container-and-patching.md`,
  "MAIN OS has a second gate").
- **Recovery is unproven.** The bootstrap's STARTUP menu (hold FUNC at
  power-on) is independent of MAIN OS, accepts SysEx over MIDI DIN only (not
  USB), and validates only a content checksum and an HMAC-SHA256 trailer —
  no version check. That a corrupted or refused MAIN OS still lets this menu
  come up is a strong inference from the code layout, **not demonstrated on
  hardware** (`docs/findings/01-container-and-patching.md`, "Recovery";
  marked **[O]**).

Read `docs/findings/01-container-and-patching.md` in full before flashing
anything.

## The hardware

Not ARM. Two processors:

| | Part | Notes |
| --- | --- | --- |
| Control | Freescale **ColdFire MCF54415** | 68k-family ISA, **big-endian**. UI, sequencer, files, MIDI. C++. |
| Audio | Analog Devices **ADSP-21569** SHARC+ | FreeRTOS. Shipped as ADI loader records. |

## The SHARC side

There is **no SHARC execution in the emulator** — nothing here emulates the
ADSP-21569 core itself. Instead:

- `tools/sharc_isa.py` is the typed encoding seam over the public-manual-derived
  `decode_table.json`: it owns deterministic form selection, normalized operands,
  and claim-level evidence references. The legacy and sharcspec decoders are
  compatibility adapters over this one model.
- `tools/sharc_trace.py` is a concrete tracer over the loaded SHARC image: it
  implements only instruction semantics taken from the public SHARC+ and
  classic SHARC programming references, refuses anything undocumented rather
  than guessing, and has been used to read back real values (poked SRC-page
  fields, compute results) from a running trace.
- `tools/sharcemu.py` adds a Ghidra-free `pypcode` backend for the same
  generated language, for faster iteration than a live Ghidra JVM.
- **Selache** ([js216/selache](https://github.com/js216/selache)), a public
  third-party VISA assembler, is used read-only as a cross-check. The optional
  `tools/sharc_selache.py` adapter requires the documented pinned revision,
  byte-swaps its parcels to boot-stream order, and returns a structured extent
  and decode comparison; `tools/selasm.py` is its command-line front end. The
  round trip through Selache has already found real bugs in our own decoder.
- The SHARC program is imported into Ghidra with the generated
  `SHARC_VISA:LE:32:default` language and dumped grep-friendly with
  `tools/ghidradump.py`; the current 1.16 dump lives at
  `out/ghidra/sharc-dt2-1.16/` (`decomp/`, `disasm/`, `xrefs.sqlite`,
  `manifest.json` — check `image_sha256`/`sections_source_sha256` there
  before trusting it).

See `docs/findings/05-sharc-isa-and-decoding.md` for the instruction-table
work and `docs/findings/06-sharc-engine-and-startup.md` for what has been
traced in the running program (the wavetable pipeline, the render call chain,
the SRC-page reads).

## Tool index

The full index, with usage details, is
**[docs/TOOLS.md](docs/TOOLS.md)**. Main tools:

| Tool | Purpose |
| --- | --- |
| `python -m emu.extract` | Extract all sections from a `.syx`. Normal entry point; `--oracle` cross-checks with the device's own depacker (1.15C/1.10E only). |
| `python -m emu.checkpoint` | Build (`make`) or resume a boot snapshot ladder. |
| `python -m emu.run` | One-command extract + build + live panel. |
| `python -m emu.gui` | The live panel on its own, against an existing snapshot. |
| `tools/roundtrip.py` | Rebuild and re-extract a container as an acceptance gate for the repack chain. |
| `tools/patchimg.py` | Apply byte-exact, preconditioned patches to an extracted section image; refuses sections 2/4. |
| `tools/emucheck.py` | Deterministic post-boot milestone check: real main screen, sane task count, no faked semaphore pends. |
| `tools/guirun.py` | Headless reproduction of the GUI's worker configuration, with hooks, input injection and PNG capture. |
| `tools/machineprofile.py` | Per-image address tables for the machine work (`dt2-1.15C`, `dt2-1.16`, `dn2-1.11`). |
| `tools/machinepatch.py` | Apply the nine-part eighth-machine patch to a *running* emulator snapshot. |
| `tools/machinebuild.py` | Build a flashable `.syx` with a new machine baked into MAIN OS. |
| `tools/machinecheck.py` | Select a machine end to end, turn its encoders, read the TX frame; compares against a baseline. |
| `tools/cavefind.py` | Find dead-byte caves in a MAIN OS image that survive a cold boot. |
| `tools/speedab.py` | Trustworthy emulator speed A/B: timing, Python-crossing census, cProfile split. |
| `tools/qemuceiling/ceiling.py` | Unicorn vs. QEMU raw ColdFire instruction ceiling, same hand-encoded loop on both. |
| `tools/sharc_trace.py` | Concrete SHARC+ instruction tracer over the loaded image, public-manual semantics only. |
| `tools/sharcemu.py` | Ghidra p-code / `pypcode` SHARC+ emulator. |
| `tools/selasm.py` | Assemble a VISA snippet with Selache and cross-check it against our own decoder. |
| `tools/ghidradump.py` | Export an analysed Ghidra program (ColdFire or SHARC+) to grep-friendly disassembly/decompilation plus SQLite indexes. |
| `tools/ghidraq.py` | Live, read-only PyGhidra queries, chainable with `--then` in one JVM. |
| `tools/hwlookup.py` | Resolve an MCF5441x address, exception vector, or eDMA channel via the cited hardware-reference contract. |
| `scratch/semscan.py` | One-off scan for `give`/`give_b` semaphore-post sites, classified ISR vs. task code (gitignored scratch tool, not packaged). |

## Layout

```
dt2/container.py   .syx -> 8-in-7 decode -> ELE3 container + section table
dt2/build.py       write-side inverse of container.py: rebuild a .syx
dt2/elz.py         byte-level aPLib-style depacker, all four firmwares
dt2/coldfire.py    ColdFire-aware disassembly (Capstone misses MVS/MVZ and FF1)
emu/harness.py     Unicorn m68k machine; works around four Unicorn/ColdFire gaps
emu/longrun.py     build() -- the machine, its models and every opt-in switch
emu/checkpoint.py  boot snapshot ladder: make / resume
emu/run.py         .syx -> running emulator, one command
emu/gui.py         live panel
emu/panel.py       the framebuffer the firmware actually draws into
emu/esdhc.py       SD/MMC controller + eMMC (identification and formatting)
emu/oracle.py      runs the DEVICE'S OWN validators against a candidate image
devices/*.toml     per-product firmware hashes and panel layout
docs/findings/     the lab notes, indexed from docs/FINDINGS.md
docs/TOOLS.md      reverse-engineering tool index and workflow guide
HANDOVER-*.md      current state and next steps -- read the newest one
```

## Continuing this work

**Read the newest `HANDOVER-*.md` in the repo root first.** It carries current
state and next steps; results belong in `docs/findings/` instead, one file per
topic, indexed from **[docs/FINDINGS.md](docs/FINDINGS.md)**. Evidence in
those files is marked **[V]** verified here, **[D]** documented or read once
but not re-checked, **[O]** open, **[C]** corrects an earlier claim.

## The acceptance oracle

The bootstrap decides whether to accept an OS image using a few self-contained
routines. Emulating them turns "will the device take this?" from a hardware
experiment into a unit test. All the pieces of that decision are now recovered
and computed, verified byte-exact against all four firmwares in the repo root
(`docs/findings/01-container-and-patching.md`):

- **CRC-32** `0x80001bd0` — poly `0xEDB88320`, residue `0xDEBB20E3`
- **Depacker** `0x80000432` — decompresses a rebuilt image *using the device's
  own code* on 1.15C/1.10E, confirming a repacked stream is acceptable without
  hardware; `dt2/elz.py` reproduces it byte-for-byte on those two and reads
  1.16/1.11 as well
- **Content checksum** (preamble) and **HMAC-SHA256 trailer** (container tail)
- **Per-packet checksum** (SysEx transport, byte 125 of each message)

`tools/roundtrip.py` runs the whole chain — rebuild, re-extract, re-check —
as the acceptance gate for the repack path. See **Hardware warning** above for
what safety this does and does not establish before flashing anything.

## Licence and attribution

**GPL-2.0-or-later.** See [LICENSE](LICENSE).

This said MIT until 2026-09-13, and could not:
the patches in `patches/` modify `qemu/target/m68k/translate.c` and
`qemu/target/m68k/unicorn.c` — QEMU source vendored inside Unicorn — so they
are derivatives of that code and carry its terms. The dependency is not
incidental either: `emu.unicorn_compat` refuses to run against stock Unicorn,
so nothing here works except against the patched build. GPL-2.0-or-later is the licence that costs nothing to be right about.

The licence covers the code in this repository and nothing else. **No Elektron
firmware is included and none ever should be** — it is copyright Elektron, and
the `.syx` you run is yours to supply. Nothing here grants any right to
Elektron's software, and nothing here is legal advice.

Not affiliated with or endorsed by Elektron. Container format knowledge derives
from `mischa85/elektron-firmware-tool` (MIT); architecture and memory-map facts
marked *Documented*/**[D]** in `docs/findings/` derive in part from
`lalzart/digitakt-ii-firmware-research-public`. The public SHARC+ VISA
assembler `js216/selache` is used read-only for cross-checking, never
vendored or quoted. MIT is GPL-compatible, so both carry forward under this
licence.
</content>
