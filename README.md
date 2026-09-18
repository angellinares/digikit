# Digitakt II firmware research

An emulator for the Elektron Digitakt II's control processor, plus tooling for
understanding and (eventually) patching its OS images.

**No Elektron firmware is included in this repository and none ever should
be** — it is copyright Elektron. You supply your own lawfully-obtained `.syx`;
`.gitignore` keeps it, the extracted sections and the snapshots out of git.

**Only `Digitakt_II_OS1.15C.syx` has been tested**, SHA-256
`62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c`. Every
address in this repo and its documentation is specific to that build. A
different firmware version will almost certainly not boot, and nothing here
tries to detect that for you.

## What it does

It boots. The main OS runs, spawns its RTOS tasks, reaches its message loop and
**renders its user interface** — pattern and project name, tempo, the encoder
parameter row, the sample page with its knob widgets — into a 128x64 panel you
can watch live, or dump to a PNG with `emu.panel`.

It is a research instrument, not a Digitakt you can play, but it is no longer
too slow to watch. Block-bounded stepping reaches roughly 10M instructions a
second — ahead of the 4.68M the timer models pace the firmware's own clock to,
though still far short of the real part's ~264M — so the panel renders at
20–25 fps and the GUI spends the surplus sleeping, keeping the emulated clock
and the wall clock together. See **What works, and what does not** below.

## Quick start

You need Python 3.12 (via [uv](https://docs.astral.sh/uv/)) and your own
firmware file in the working directory.

```sh
uv sync
tools/install-patched-unicorn.sh

# Digitakt II
uv run python -m emu.run Digitakt_II_OS1.15C.syx

# Digitone II
uv run python -m emu.run Digitone_II_OS1.10E.syx
```

That checks each prerequisite, builds the boot snapshots on first run (one cold
boot from reset, a few minutes — it happens once), and opens the live panel.

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
`count=` stepping, about 5x slower, and is what to use when comparing a run
against `tools/bootcheck.py`.

**One firmware at a time.** `sections/` holds the decompressed image of
whichever `.syx` was extracted last, and the filenames are fixed, so switching
between the two devices re-extracts. `emu.run` checks this and refuses rather
than running one firmware under the other's name; do what it tells you. To run
one build while another occupies `sections/`, point `DT2_SECTIONS` at a second
directory instead.

Unicorn needs the repo's patches (SR read and CCR sync, EMAC MAC with load);
see [docs/UNICORN.md](docs/UNICORN.md).
`uv sync` can restore stock Unicorn, which the emulator deliberately rejects
until the installer is rerun.

That includes decompressing the sections out of the `.syx`, which no longer
needs an outside tool. To do it on its own:

```sh
uv run python -m emu.extract Digitakt_II_OS1.15C.syx -o sections/
```

The decompressor is `dt2/elz.py`, a byte-level decoder for the device's codec
written from the format in
[elektron-firmware-tool](https://github.com/mischa85/elektron-firmware-tool)'s
`aplib.c`. It reads every firmware in the repo root, 1.16 and 1.11 included.
`--oracle` uses the device's own routine instead: section 4 is the *updater*,
it is stored **raw**, and an updater has to unpack the image it installs, so
it carries its own copy of the depacker; `emu/extract.py --oracle` runs that
copy under Unicorn. Both are byte-identical to `emu.oracle.depack` — the
device's own section-2 routine — for every compressed section of Digitakt II
1.15C and Digitone II 1.10E. The oracle cannot read 1.16 or 1.11.

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

Nothing is hardcoded to a filename. Paths resolve from an explicit argument,
then an environment variable, then discovery:

| | |
| --- | --- |
| `DT2_SYX` | the firmware `.syx` |
| `DT2_SECTIONS` | directory of extracted sections (default `sections`) |
| `DT2_SNAPSHOTS` | directory of boot snapshots (default `snapshots`) |
| `DT2_MAIN_IMG` | the decompressed MAIN OS image |

The section filenames are fixed, so `sections/` holds exactly one firmware at a
time. `emu.run` records which `.syx` it came from and refuses to pair it with a
different one — otherwise a second firmware silently runs against the first
one's code. Snapshots for anything other than the tested build go in their own
subdirectory for the same reason.

## What works, and what does not

**Working:** the ColdFire core, the interrupt controllers, the PIT and DMA
timers, eDMA, the front-panel link, the coprocessor port's handshake, the
display, and the SD/MMC controller through card identification.

**Storage is not supported yet.** The eSDHC controller and an eMMC are modelled
far enough to complete card identification and read EXT_CSD, but **no block
data is served** — `CMD18` reads return zeros. Bulk transfers move through the
SoC's eDMA with `SADDR = DATPORT`, and nothing backs them. So the firmware
boots and draws, but cannot load a project or samples, and anything that
touches the filesystem will not work. Backing it with a real image, and then
generating one from a folder of samples, is the next substantial piece of work.

**No audio.** The device makes sound on a second processor — an Analog Devices
ADSP-21569 SHARC+ running its own firmware. Nothing here emulates it. The main
OS holds parameter state and RPCs it to the SHARC; that split is what makes
audio a separate project rather than a missing feature.

**No input.** The front-panel protocol is decoded in the transmit direction
only. Buttons and encoders arrive on the receive side, which is not modelled,
so the UI cannot be driven.

## The hardware

Not ARM. Two processors:

| | Part | Notes |
| --- | --- | --- |
| Control | Freescale **ColdFire MCF54415** | 68k-family ISA, **big-endian**. UI, sequencer, files, MIDI. C++. |
| Audio | Analog Devices **ADSP-21569** SHARC+ | FreeRTOS. Shipped as ADI loader records. |

## Layout

```
dt2/container.py   .syx -> 8-in-7 decode -> ELE3 container + section table
dt2/coldfire.py    ColdFire-aware disassembly (Capstone misses MVS/MVZ and FF1)
emu/harness.py     Unicorn m68k machine; works around four Unicorn/ColdFire gaps
emu/longrun.py     build() -- the machine, its models and every opt-in switch
emu/run.py         .syx -> running emulator, one command
emu/gui.py         live panel
emu/panel.py       the framebuffer the firmware actually draws into
emu/esdhc.py       SD/MMC controller + eMMC (identification only)
emu/oracle.py      runs the DEVICE'S OWN validators against a candidate image
docs/HANDOVER.md   current state, what to do next, and the traps that cost time
docs/TOOLS.md      reverse-engineering tool index and workflow guide
```

## Other tools

See **[docs/TOOLS.md](docs/TOOLS.md)** for the full tool index, including the
Ghidra, SHARC+ loader/decoder, targeted data-flow, patching, and measurement
workflows.

```sh
uv run python -m dt2.container Digitakt_II_OS1.15C.syx   # section table
uv run python -m emu.panel <snap> <instrs> 3 out.png     # render the panel
uv run python -m emu.uiprobe sweep                       # the measurement sweep
uv run python -m emu.tasks <snap>                        # parked PC per task
uv run python emu/oracle.py                              # acceptance oracle
```

## Continuing this work

**Read [`docs/HANDOVER.md`](docs/HANDOVER.md) first.** It is written for someone
with no memory of the sessions that produced this, and it opens with six
standing warnings about measurements that have already misled people — several
of them cost a whole session each. [`docs/NEXT.md`](docs/NEXT.md) is the
overview and [`docs/FINDINGS.md`](docs/FINDINGS.md) the older evidence.

## The acceptance oracle

The bootstrap decides whether to accept an OS image using a few self-contained
routines. Emulating them turns "will the device take this?" from a hardware
experiment into a unit test. Two are implemented and verified byte-exact:

- **CRC-32** `0x80001bd0` — poly `0xEDB88320`, residue `0xDEBB20E3`
- **Depacker** `0x80000432` — decompresses a rebuilt image *using the device's
  own code*, confirming a repacked stream is acceptable without hardware

Still to add: the content checksum at `0x40003ca6` and the HMAC-SHA256 trailer
check at `0x80005e2a`. Together those are the bootstrap's entire accept/reject
decision.

## Safety

Established by static analysis and emulation, not by flashing anything:

- The **recovery path** is the bootstrap's STARTUP menu (hold FUNC at
  power-on), independent of MAIN OS, accepting SysEx over **MIDI DIN only** —
  not USB.
- The only irreversible operation is **BOOTSTRAP UPGRADE**, gated at
  `0x80001c72` by `bcc` — it runs only when the incoming bootstrap version is
  *strictly greater* than the running one (`0x0200` in 1.15C). Patching
  sections 3/7 never presents a higher version, so that step is unreachable.
- **Never modify sections 2 (bootstrap) or 4 (updater).** A patcher should
  hard-fail on any patch targeting them.

Read `docs/FINDINGS.md` before flashing anything.

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
marked *Documented* in FINDINGS.md derive from
`lalzart/digitakt-ii-firmware-research-public`. MIT is GPL-compatible, so both
carry forward under this licence.
