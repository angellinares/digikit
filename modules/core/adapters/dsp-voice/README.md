# DSP voice hook

**Category:** core  **Devices:** Digitakt II 1.17; Digitone II 1.12  **Status:** experimental (emulator only)

A framework module: it lets other modules render a machine's sound with their own DSP routine (`digikit/pluck` is the
example). On its own it changes nothing: a patched image with no registered machine plays exactly the stock audio.

- What changes on the device: nothing you can hear. The first instruction of the DSP's per-voice render function is
  replaced by a jump into this module, which looks the voice's machine type up in a table and runs the registered routine,
  or continues in the stock code.
- Cost without a registered machine: voice 0 builds a mask of the registered types once per block (type t sets bit t; 31 or more set bit 31; free rows
  of `core/dyndsp` are no type). With an empty mask every other voice goes back to the stock code after one check (about 10 instructions instead of
  about 100). With a registered type a voice looks its own type up in the mask and runs the table scan only on a hit. A row that the runtime writes
  takes effect from the next block. On the Digitakt II the voices write no breadcrumb while the mask is empty.
- The DSP records, once per block, a heartbeat and the context registers it found (see `core/dsp-link`, `dl_diag`).
  It also writes a breadcrumb (the step, the voice and the machine type) before each step of a registered machine's path, and counts the calls and
  returns of the routines, so that a halt inside a routine names its place ([docs/findings/48-dsp-breadcrumbs.md](../../../../docs/findings/48-dsp-breadcrumbs.md)).
- Build option `DV_BISECT_C2` (not part of any shipped image): types of 7 or more take the stock path with the breadcrumb written inline and are never dispatched. It exists to
  separate the wrapper's call path from the stock pipeline on a device (docs/PATCHING.md "Rung C2", `tools/patchkit/pk_variant.py`).
- Build options `DV_C3_FINE`, `DV_C3_DB`, `DV_C3_NOIRQ`, `DV_C3_NOSYS` (not part of any shipped image): the rung C3 variants of finding 49 section 9 (fine breadcrumbs 8 to 10 on the registered path,
  delayed `CALL`/`RTS`, interrupts masked across the registered path, no MODE1 write). docs/PATCHING.md "Rung C3".
- Limits: at most 16 registered types; the Digitakt II's Stretch machine (type 2) is not hooked; type 0 cannot be registered.

## The runtime guard (Digitakt II, runtime 5)

Around a registered routine the wrapper checks what it cannot trust (docs/findings/51-pluck-tune-guard.md):

- Before the call the eight machine knob words of the track are clamped in the frame copy to the ranges the machine's `[[param]]` declare (`min`/`max`; a stock knob
  or an undeclared one: 0 .. 0x7f00) and are put back after the call. The ranges are in the voice row (24 words, see the `[[registry.field]]` comment in `module.toml`).
- After the call the block must hold finite samples of at most 8.0 in magnitude. Otherwise it is zeroed, the voice ends (ACTIVE and the mark of the record clear), the fault is counted
  and reported in `dl_diag` words 17 to 23 and the log (`MACHINE_FAULT`). A type that faults 8 times within 1500 blocks is disabled: its voices render silence and the routine is not
  called until the next start (`MACHINE_DISABLED`).
- Cost: about 400 instructions per registered even voice and block on the rig (0.07 % to 0.09 % of the DSP).
- A row with a `note_on` pc (row words 16 to 19: `note_on`, `state_base`, `stride`, `clear`) is a machine of the SDK contract (`<dk/machine.h>`) on both devices: the adapter finds the note-on, clears the voice's state, calls `note_on` and `render` with the voice input and the parameter words, ends the voice on `DK_DONE` or a fault, and applies the guard above. On the Digitone II the guard is the same, a faulted slot is silent until the next trig. A row without a `note_on` is a routine of the form below and runs as described.

- Stereo machines (machine contract 1.2): the status of `render` may carry `DK_STEREO`. The adapter gives `dk_voice_in` an `out_r` (the odd voice's block of the pair on the Digitakt II, the slot's R buffer on the Digitone II). On the Digitakt II the even voice sets a flag for its track; the odd voice keeps the R the machine wrote instead of copying L, and clears the flag (the even voice clears it at the start of every block as well). The guard of a stereo block covers both blocks: a bad sample in either zeroes both and ends the voice. A mono machine behaves as before. Digitone II: the adapter checks R (finite, at most 8.0; a bad R block plays R = L for the block), scales it like the engine scales L and stamps the slot; every stage of the engine between the machine and the pan (the shaper, the first-order, held-sample and U/Y stages, the filter types of the dispatch including the ring stage with its own R ring, the gain stage, the block ramp and the fade) also runs on R with a private state, the resets in the middle of a note are mirrored, and the pan stage feeds plane B from R. A context a twin cannot run in (a circular buffer or a block of other than 32 samples) makes the slot play R = L for the block; it is counted per stage (`_gs_cnt`) and raised in `_gs_health`. At most 8 stereo slots per block (docs/design/dn2-stereo.md).

A routine should therefore render silence for an idle voice and start at its note-on path when byte `+0x1b9` of the record is clear.

## For module authors: the routine's register contract

The firmware restores a number of registers per DSP context, so the wrapper never assumes their values: before a registered
routine runs it saves them and **sets** them, and it puts the caller's values back when the routine returns.

| register | value while the routine runs |
| --- | --- |
| M5, M6, M7 | 0, 1, -1 (a post-modify of `M6` steps one word; `M7` steps back one) |
| M13 | 0 (`JUMP (M13, I13)` is the return) |
| M14, M15 | 1, -1 |
| L0-L5 | 0 (linear addressing for I0-I5) |
| MODE1 | SIMD off, ALUSAT and TRUNCATE clear, RND32 set (32-bit floats, round to nearest) |

The routine may clobber R0-R15, I0-I5, I13, M1-M4 and B3, and must keep I6-I12, I14, I15, M0, M5-M15, the L registers and
MODE1 as it found them (the pinned values above). Details, the entry registers and the voice record: the header of
`src/dsp/dsp-voice.s` (Digitakt II) and `src/dsp/dsp-voice-dn2.s` (Digitone II); the audit and its tests:
[docs/findings/45-dsp-voice-context.md](../../../../docs/findings/45-dsp-voice-context.md).

The gallery shows the text above the rule. Keep it free of firmware bytes and vendor tool names.
