# Audio suite: one set of audio readers

Three tools read rendered audio today. This note says what each one does, and how they become one set in
`tools/emulator/`, in Python.

## The three tools

Each tool answers a different question:

- **dn2_firmware `dnfw wav`:** "Is this oscillator clean, and did this render change?" It reads a held note.
- **moveforge `palette.ts`:** "What does this module sound like, and does each control do something?" It reads
  single hits, knob sweeps and presets, for a person to listen through.
- **digikit `audiocheck.py`:** "Did the emulator run do what the scenario expects?" It reads a whole run, mostly
  for effects: echoes, decay, sample-rate reduction.

| Measure | `dnfw wav` | `palette.ts` | `audiocheck.py` | In this change |
|---|---|---|---|---|
| Pitch, and the power between its harmonics | **yes** | no | harmonic levels of a decaying tone | `audiotone` |
| Power above 20 kHz | **yes** | no | no | `audiotone` |
| Level: peak, RMS, DC, clipping | peak, RMS, crest, DC, clipped samples | peak, crest | peak, RMS, DC, non-finite samples, growth | `audiocheck` (as now) |
| Clicks (isolated steps) | **yes** | no | onsets | `audiotone` |
| Two renders, sample for sample | **yes** | no (a separate golden check with tolerances) | no | `audiotone` |
| Spectrum | averaged Hann, or Blackman-Harris for fine detail | 48 log bands from the onset | Hann over the whole file, with peaks | each keeps its own |
| Spectrogram | PNG sheet with waveform and close-up | no | PNG | `audiocheck` (sheet later) |
| Decay | no | T60 of a 5 ms RMS envelope | RT60 (Schroeder), slope | both kept |
| Attack, centroid, brightness, flatness | no | **yes** | centroid per segment | `palette` |
| Stereo | correlation, balance | width (side over mid) | L-R difference | each keeps its own |
| Knob sweeps and dead controls | no | **yes** | no | `palette` |
| Presets that are the same sound | no | **yes** | no | `palette` |
| Echoes, sample-rate images, bit depth, modulation rate | no | no | **yes** | `audiocheck` (as now) |
| Output | JSON, PNG sheet, plain findings | Markdown and an HTML page with audio on every row | features.json, PNGs | JSON, findings, Markdown, HTML |

### Where they overlap and where they differ

- **What one reading covers.** `dnfw wav` reads the loudest half second of a held note. `palette.ts` reads the
  first 250 ms of a hit from its onset. `audiocheck.py` reads the whole run. A measure moved from one tool to
  another keeps its own window, because the window changes the answer.
- **Spectrum.** All three take one, each for its own job. `palette.ts` draws a shape for the eye.
  `audiocheck.py` finds peaks. `dnfw wav` needs side lobes far below the noise to see what lies between
  harmonics. This change keeps all three and does not merge them into one.
- **Decay.** `palette.ts` and `audiocheck.py` both measure it, in two ways. T60 of an RMS envelope suits a
  single hit. Schroeder RT60 suits a reverb tail. Both stay.
- **Stereo.** Width, correlation and L-R difference each show a different fault. Width shows a spread control
  that does nothing. Correlation shows a phase fault. L-R difference shows that two channels that should match
  do not.
- **Change against a reference.** `dnfw wav compare` asks "are these the same samples?". moveforge's golden
  check asks "is this within tolerance of last time?". digikit's scenario oracles ask "does this feature have
  the expected value?". The three checks are three levels of strictness, and each has its use.
- **Who reads the result.** `dnfw wav` and `audiocheck.py` write numbers for a test or an agent. `palette.ts`
  writes a page for a person to listen through. Only `palette.ts` lets the reader hear each row.

## Why Python

- digikit's audio readers are Python on numpy (`audiocheck.py`, `live_audio.py`, the SHARC harness).
- The renders come from Python and Rust tools: the coupled runner, the SHARC harness, and recordings from a unit.
- numpy gives the FFT. `palette.ts` carries its own FFT.

The palette page stays HTML. Python writes it.

## What this change adds

- `tools/emulator/audiotone.py`: a held note. Pitch, `between_db`, `above_db`, `steps`, `compare` and
  `note_report`. These come from dn2_firmware's `dnfw.audio.metrics`.
- `tools/emulator/palette.py`: the palette. `describe`, `fingerprint`, `travel`, `is_dead`, `same_voice`, and
  `palette.md` and `palette.html` from a manifest of WAVs. The measures, the thresholds and the report follow
  `palette.ts` and `lib/descriptors.ts` (moveforge 3edfa17).
- `tests/dsp/test_audiotone.py` and `tests/dsp/test_palette.py`, on synthetic signals.

## The polyBLEP table as a regression test

dn2_firmware chose a polyBLEP for the Waverider sub on one measure. That measure is the power between the sub's
harmonics, note by note, with and without the polyBLEP. `between_db` is that measure. The test rebuilds both columns of that
table to 1 dB, from a naive square and a textbook polyBLEP square:

| Note | Sub Hz | Naive | polyBLEP |
|---|---|---|---|
| A3 | 110 | -27 dB | -39 dB |
| C5 | 262 | -24 dB | -40 dB |
| A5 | 440 | -26 dB | -63 dB |
| C7 | 1,047 | -17 dB | -32 dB |

A change to the measure that reads the table differently fails the test. As a control, a harmonic band of
±0.05 × f0 in place of ±0.12 × f0 moves 6 of the 14 notes by more than 1 dB.

`palette.ts` cannot make this reading. It does not detect pitch, and its spectrum bars and flatness show a bright
square with or without aliasing. It also reads the first 250 ms of a hit, which suits drums but not a held note.

## Next steps

1. dn2_firmware: `dnfw wav` calls these modules in place of its own copies. dn2_firmware has two definitions of
   "between the harmonics" (`metrics.inharmonic_db` and the page script's `_between_db`). The one in
   `audiotone.between_db` is the one that the polyBLEP decision used, and the other one goes.
2. A renderer writes a palette manifest: the coupled runner for a machine, the SHARC harness for a DSP module.
   The sweep needs each parameter's range and default from the module's own description.
3. dn2_firmware's plot sheet (waveform, close-up, spectrum, spectrogram) joins `audiocheck.plots`.

## Open question

moveforge has no licence file. `palette.py` is new Python code, but it follows `palette.ts` closely. Its author
must agree to this before the merge, or give moveforge a licence that is compatible with AGPL-3.0.
