# Digitakt II firmware research

This repository contains two things:

- An emulator for the control processor (ColdFire) of the Elektron Digitakt II
  and Digitone II. It boots the real OS and shows its screen.
- Static analysis tools for the audio processor (SHARC+ DSP). They decode the
  DSP program, store it in a database, and trace code.

The goal is to understand the firmware well enough to add new machines and
sounds to both devices.

**No Elektron firmware is in this repository.** The firmware is copyright
Elektron. You supply your own `.syx` file. `.gitignore` keeps the firmware,
the extracted sections, the snapshots and the databases (`out/`) out of git.

## Firmware

Use Digitakt II OS 1.16 and Digitone II OS 1.11. The older builds are kept
because much of the early analysis used them.

| device | version | file | sha256 |
| --- | --- | --- | --- |
| Digitakt II | 1.16 | `Digitakt_II_OS1.16.syx` | `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec` |
| Digitone II | 1.11 | `Digitone_II_OS1.11.syx` | `2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0` |
| Digitakt II | 1.15C | `Digitakt_II_OS1.15C.syx` | `62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c` |
| Digitone II | 1.10E | `Digitone_II_OS1.10E.syx` | `9881ee5ca4624be2b833e5eb94e294f2f8425371e6729781baccd1c256cac504` |

Keep each firmware's extracted sections in its own directory:
`out/sections/dt2-1.16`, `out/sections/dn2-1.11`, and so on. The SHARC tools
find images by these names. A sections directory records the SHA-256 of its
`.syx` in `.source-sha256`, and the tools refuse to mix firmwares.

## Setup

You need [uv](https://docs.astral.sh/uv/) (Python 3.12).

```sh
uv sync
tools/install-patched-unicorn.sh   # the emulator needs a patched Unicorn
```

Run `tools/install-patched-unicorn.sh` again after `uv sync` if the emulator
reports stock Unicorn. See [docs/UNICORN.md](docs/UNICORN.md).

## Run the firmware in the emulator

One command extracts the firmware, builds boot snapshots on the first run (a
few minutes), and opens the live screen:

```sh
uv run python -m emu.run Digitakt_II_OS1.16.syx
uv run python -m emu.run Digitone_II_OS1.11.syx
```

No flags are necessary. Useful options:

| option | effect |
| --- | --- |
| `--check` | Check the files and settings, then stop. |
| `--exact` | Exact instruction counting (5-7x slower). Use it to compare runs. |
| `--scale N` | Screen zoom. |
| `--weakptr` | Work around a boot stall in `weak_ptr::lock`. Use only if the boot stops. |

### Step by step

Use these commands when a tool needs explicit paths:

```sh
# 1. Extract the sections (about 1 second).
uv run python -m emu.extract Digitakt_II_OS1.16.syx -o out/sections/dt2-1.16

# 2. Build the boot snapshots (about 6 minutes).
DT2_SECTIONS=out/sections/dt2-1.16 DT2_SNAPSHOTS=out/snapshots/dt2-1.16 \
  uv run python -m emu.checkpoint make \
  60000000,120000000,200000000,280000000,400000000 \
  out/snapshots/dt2-1.16/boot Digitakt_II_OS1.16.syx

# 3. Check that the boot reached the real main screen.
uv run python tools/emucheck.py --device dt2 --syx Digitakt_II_OS1.16.syx \
  --sections out/sections/dt2-1.16 \
  --snapshot out/snapshots/dt2-1.16/boot400M.snap --instrs 600000000

# 4. Open the live screen from a snapshot.
DT2_SECTIONS=out/sections/dt2-1.16 \
  uv run python -m emu.gui out/snapshots/dt2-1.16/boot400M.snap \
  --syx Digitakt_II_OS1.16.syx
```

For Digitone II, use `--device dn2`, `Digitone_II_OS1.11.syx` and
`dn2-1.11` in the paths.

The tools find files from an argument first, then from these variables:
`DT2_SYX` (the `.syx`), `DT2_SECTIONS`, `DT2_SNAPSHOTS`, `DT2_MAIN_IMG`
(the decompressed MAIN OS).

### What the emulator can and cannot do

| Works | Does not work |
| --- | --- |
| The OS boots on all four builds and shows its user interface. | No audio. The emulator does not run the SHARC DSP. |
| The screen runs at about 20-25 frames per second. | No real storage data. The +Drive is always new and empty. |
| Test tools can press keys and turn encoders (`tools/guirun.py`, `tools/machinecheck.py`). | No live keyboard or encoder input from a person. |
| Timers, interrupts, DMA, the front-panel link, the display and the SD/MMC controller. | |

## Analyse the SHARC DSP

The emulator does not run the SHARC. Instead, the tools decode the DSP
program into a database, and you query and trace it. The DSP program is
section 7 of the firmware. Extract it first (step 1 above).

### Build the database

```sh
uv run python tools/sharcdb.py build out/sections/*/section_7_BLOB.bin
```

This takes about 15 seconds for all four images. It writes
`out/sharcdb/<image>.sqlite` (for example `dt2-1.16.sqlite`). It skips an
image that is already current. The database contains:

- instructions, functions, call and jump edges, basic blocks and loops
- literals, memory accesses, data references and resolved pointers
- register definitions and uses
- roots (boot entry, RTOS tasks, interrupt vectors, code-pointer tables),
  reachability, call graph and dominators
- function hashes that match the same function across images

To add a ColdFire image from a Ghidra dump:
`uv run python tools/sharcdb.py import-ghidra out/ghidra/dt2-1.16-emac --name dt2-1.16-cf`.

### Query it

Use `tools/sharc.py` in one short Python script:

```python
import sys; sys.path.insert(0, "tools")
import sharc

img = sharc.load("dt2-1.16")      # builds the database if necessary
img.func(0x1c6553)                 # the function that contains an address
img.listing(0x1c642a, 20)          # 20 instructions from an address
img.callers(0x1c2b24)              # call and jump sites into a function
img.card(0x1c06ba)                 # a short summary of one function
img.writers(0x250738)              # instructions that store to an address
img.last_def("R6", 0x1c6553)       # last writer of a register
img.reach(0x1c642a, 0x1c7053)      # can one address reach another?
img.sql("SELECT kind, count(*) FROM roots GROUP BY kind")
```

For one SQL query from the shell:

```sh
uv run python tools/sharc.py dt2-1.16 "SELECT kind, count(*) FROM roots GROUP BY kind"
```

Canned queries are in `tools/sharcdb.sql`. Function notes (role, summary,
confidence) are kept in `out/sharcdb/<image>.notes.sqlite` and appear in
`img.card()`.

### Trace code

`tools/sharc_trace.py` runs SHARC code from the real firmware bytes. It uses
only instruction semantics from the public manuals. It stops on anything
undocumented instead of guessing.

```sh
uv run python tools/sharc_trace.py out/sections/dt2-1.16/section_7_BLOB.bin \
  --blob --start 0x1c06ba --concrete-memory --assume-32bit-normal-words \
  --set R4=0x3f800000 --set R8=0x40000000 --approx-recips --summary
```

From Python, `img.trace(start, R4=..., R8=...)` does the same and sets the
firmware's fixed M registers for you.

### What is known about the DSP

The results are in
[docs/findings/06-sharc-engine-and-startup.md](docs/findings/06-sharc-engine-and-startup.md).
In short, for Digitakt II 1.16:

- An RTOS audio task renders each audio frame. It reads a command from the
  ColdFire and converts the input and output rings.
- Each track has a machine type. A remap table converts it to a selector. The
  selector chooses the voice trigger in `FUN_1c642a`.
- 32 voice records play samples with interpolation. Then the audio goes
  through the per-track effects, the sends, and the master stage.
- The layout of the voice record (step, positions, flags, buffers) is
  verified. A new sound must use this layout.

## Patch the firmware

`tools/machinebuild.py` adds an eighth machine to the ColdFire OS (section 3)
and writes a new `.syx`. The new machine is a copy of an existing machine with
different parameter fields. This changes only the ColdFire side. What the
DSP does with the new machine type is not verified yet: its remap table
covers machine types 0-6.

```sh
uv run python tools/machinebuild.py --syx Digitakt_II_OS1.16.syx \
  --profile dt2-1.16 --machine XSLICE:XSL:6:7 \
  --fields 0xf8,0xf9,0xfa,0xfb,0xfc,0xfd,0,0xfe,0x0a \
  --sections-dir out/sections/dt2-1.16 --out out/Digitakt_II_OS1.16.xslice.syx
```

To check the result in the emulator, extract the new `.syx`, build a
snapshot, and select the machine with `tools/machinecheck.py`. See
[docs/findings/02-machines-and-parameters.md](docs/findings/02-machines-and-parameters.md)
for the steps, and `tools/cavefind.py` to find free space.

`tools/roundtrip.py` rebuilds and re-extracts a container, and checks the
same checksums and HMAC that the device checks
([docs/findings/01-container-and-patching.md](docs/findings/01-container-and-patching.md)).

### Hardware warning

- No patched image from this repository has been installed on a device.
  Only Em decides to do that.
- Never change section 2 (bootstrap) or section 4 (updater).
  `tools/patchimg.py` refuses to do it.
- Recovery through the bootstrap STARTUP menu (hold FUNC at power-on, SysEx
  over MIDI DIN) is not tested on hardware.
- Read `docs/findings/01-container-and-patching.md` before you install
  anything.

## Hardware

| | Part | Notes |
| --- | --- | --- |
| Control | Freescale ColdFire MCF54415 | 68k family, big-endian. User interface, sequencer, files, MIDI. |
| Audio | Analog Devices ADSP-21569 SHARC+ | FreeRTOS. The program is an ADI boot stream in section 7. |

## Tools and layout

The full tool index is [docs/TOOLS.md](docs/TOOLS.md).

```
dt2/            .syx container: decode, rebuild, decompress (elz.py)
emu/            the ColdFire emulator (run, gui, checkpoint, extract)
tools/          analysis tools; SHARC: sharcdb.py, sharc.py, sharc_trace.py
devices/*.toml  firmware hashes and panel layout per device
docs/findings/  results, one file per topic, indexed from docs/FINDINGS.md
HANDOVER-*.md   current state and next steps (read the newest)
```

## Continue the work

Read the newest `HANDOVER-*.md` first. Record results in `docs/findings/`,
not in handovers. The marks in the findings are: **[V]** verified here,
**[D]** documented or read once, **[O]** open, **[C]** corrects an earlier
claim.

## Licence

GPL-2.0-or-later. See [LICENSE](LICENSE). The patches in `patches/` change
QEMU source inside Unicorn, so they carry the GPL.

The licence covers the code in this repository only. It gives no rights to
Elektron's software. This project is not affiliated with Elektron. The
container format knowledge comes from `mischa85/elektron-firmware-tool`
(MIT). Some architecture facts come from
`lalzart/digitakt-ii-firmware-research-public`. The public SHARC+ assembler
`js216/selache` is used only as a read-only cross-check.
