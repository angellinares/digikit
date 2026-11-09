# TB303

Open303 (Robin Schmidt, MIT license: `LICENSE-Open303.txt`) as a run-time Digitakt II machine: the 303 saw or square through
Open303's TB-303 filter, its filter envelope and accent, and its amp envelope and output stage. Set the track's AMP page to a gate (attack 0, hold or decay long, sustain full, release
short): it still multiplies the machine and retriggers on every trig. The FLTR page still filters after the machine.

| knob | |
|---|---|
| A TUNE | stock |
| B WAVE | 0 saw, 1 square |
| C ACC | accent amount, 0 to 100 % (Open303's accent knob) |
| E CUT | cutoff, 314 to 2394 Hz (Open303's knob) |
| F RES | resonance |
| G ENV | env mod |
| H DEC | filter envelope decay, 200 to 2000 ms |

- **Accent:** a trig with velocity above 100. ACC sets how much: the accent sweep and the louder amp scale with it; the 200 ms
  filter decay and the 50 ms release apply to every accented note, as in Open303.
- **Level:** DEC sits on knob H, the stock LEV knob, so TB303 tracks play at a fixed level of 100 (core/dsp-voice gives a machine
  with a parameter on knob 7 that level). AMP VOL and the kit's track level still set the volume.
- **Slide (a tie):** a note slides from the previous pitch when the previous note is still held as it starts (Open303's 60 ms
  slide). Neither the amp nor the filter envelope restarts; the new note sets the accent, the filter decay and the release.
  In the sequencer that means a trig whose LEN reaches the next trig. Set the track's LEN below 1 step (for example 0.5, a
  303's gate) and lengthen the trigs that should slide. Live in KEYBOARD mode: play legato.

Differences from Open303:

- Runs at 48 kHz without Open303's 4x oversampling and its elliptic decimator. The oscillator level is the table increment's
  exponent + 1 (Open303: + 2), and nothing above 24 kHz is made. Against Open303 itself (`tb303_ref.Open303`, checked against
  its C++ to 1e-7) the spectra differ by 0.1 to 0.5 dB (log-spectral distance, test_tb303.py).
- b0's division is a reciprocal by three Newton steps from a quadratic guess (2e-7).
- The filter's coefficients are computed once per block (32 samples, 0.67 ms) at its last sample and ramped linearly, where
  Open303 computes them every sample. A new note's first block starts on its own coefficients, with no ramp from the last
  note's. Over a run of notes the spectra differ from the per-sample version by 0.004 to 0.14 dB (log-spectral distance; the
  per-sample version is 0.08 to 0.37 dB from Open303), and a note's first 2 ms by at most 3 % of its peak.
- Cutoff, env mod and resonance glide to the knob values with a 2 ms time constant. Abrupt changes every block (an LFO's square,
  p-locks) make Open303's filter blow up, at 4x too; the glides keep it bounded.
- The amp is Open303's: its envelope (instant attack, 1230 ms decay to 0 while held, 1 ms release, 50 ms after an accent) plus
  (0.45 + 4 accent) env while held, through its declicker. Output gain -6 dB (the plug-in's default volume).
- One voice per track; the phase, the filters and the envelope run on across notes (Open303 resets them only when idle).
- The waveforms take 51 KB instead of 197 KB, so the module fits the runtime's 64 KB stage: level 0 (Open303's raw prototype, read
  only below about 23 Hz) plays level 1, and levels 3 to 11 are stored shorter (exact resamplings, at least 8 samples per cycle of
  their top harmonic).

The slide and the amp's release read `in->gate` (sdk/sdk.toml [machine]): 0 from the note's release on. The state is never cleared
by the adapter, and the runtime does not clear it at load, so a voice's first note-on zeroes it.

Cost: 8,037 instructions per block including the adapter, the same for any note and setting (8,241 on a note-on block).

Tables and constants: `uv run python modules/machines/tb303/src/dsp/gen_tables.py` writes `src/dsp/tables.inc`. Model:
`tests/tb303_ref.py` (`Tb303`); the routine matches it exactly on the SHARC core (`tests/test_tb303.py`).

Pack: `mise run module:pack -- tom/tb303 --device dt2`.
