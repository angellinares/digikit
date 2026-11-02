# SHOT

digikit/shot: a stereo sample-playback machine for the Digitakt II and the Digitone II, loaded at run time. It plays one entry of its sample pool
once per note (a one-shot) through the per-module pool of API 1.5 (`<dk/sample_track.h>`): 16 entries, kept per project. The parameter SMP
picks the entry. The picker adds PCM16 files (mono or stereo) to the pool through the runtime and sets SMP to the chosen entry; SHOT reads the pool.

| Word | Parameter | Knob | Range and default | Meaning |
| --- | --- | --- | --- | --- |
| 0 | SMP | B | 0 to 15, default 0 | pool entry that plays; turning steps through the pool, p-lockable; an empty entry is silent |
| 1 | START | C | 0 to 127, default 0 | first frame, spread over the whole sample: frame = round(frames * v / 127) |
| 2 | END | E | 0 to 127, default 127 | end frame (exclusive) the same way; END at or below START plays nothing |
| 3 | REVERSE | F | 0 to 127, default 0 | 64 and above plays backwards from END - 1 to START |
| 4 | PITCH | G | 0 to 127, default 64 | 64 is the played note unchanged; each unit is 3/8 semitone, so -24 to +23.6 semitones |

The knob letters are the same on both devices (the shared knob map of `param_slots`: words 0 to 4 on B C E F G). The Digitakt II keeps TUNE on
A, the sample slot on D and LEV on H; the Digitone II has A, D and H free.

- Pool: click SMP (release without a turn) to open the list: `+ ADD`, then the entries 00 to 15 with name and length. The entry SMP points at is
  marked with `>` and the cursor starts on it. YES on an entry sets SMP to it and returns to the page. YES on `+ ADD` opens the folder browser; YES
  on a file adds it to the pool, sets SMP to the new entry and returns to the page, all in one press. An error (pool full, bad path) closes the
  list and shows its text on the page. RIGHT on an entry gives CLEAR and INFO; NO goes back one level (browser, list, page). The pick writes SMP of the
  clicked track with `param_set` (API 1.6); while TRIG or FUNC is held it writes nothing and the page shows `RELEASE TRIG TO SET SMP`. The list is the
  chooser of `docs/design/ui-rules.md`. SHOT declares `DK_PARAM_ACTION(shot, 0, shot_smp_click)`; the runtime page hook calls
  `void shot_smp_click(uint32_t track)` in the UI task (see `docs/design/machine-page-click.md`). The test route `test/track-select` fills the pool
  in tests.
- Pitch: MIDI note 60 plays the sample at its own speed; the cursor rate is `srcrate / 48000 * 2^(semitones / 12)` in Q16.16, limited to 1/65536 to 64
  frames per sample. As in the stock one-shot, SMP is read at note-on; START, END, REVERSE, PITCH and the note follow every block. A REVERSE
  change flips the direction at the current cursor. Forward play reads from the cursor to END (a later START has no effect); reverse play reads
  down to START. An END moved behind the cursor (or START above it in reverse) ends the voice in that block with the end ramp.
- Stereo: a stereo sample gives distinct L and R through machine contract 1.2 (`out_r`). A mono sample plays centred (`base_r = base_l`). On the
  Digitakt II R goes to the odd voice of the stock voice pair. On the Digitone II L and R are both heard: all pre-pan stages have R twins
  (`docs/design/dn2-stereo.md`, phase 2); the fallback to R = L is only a guard.
- Lifecycle: `note_on` takes the pool index from the SMP word and copies that entry's `dk_strack` record under the admission and epoch macros; every
  `render` checks the epoch and acquires and releases the entry. A stale token (clear, a new load, a stock reset) or an entry that is not ready gives
  a silent block and `DK_DONE`.
- Packing: `patchkit pack digikit/shot --device dt2|dn2` carries the pool table in the DSP section (kind 2 record) and declares API 1.5 and machine
  contract 1.2. The runtime reserves it when the module loads and writes the records over the DSP link once the DSP image is in.
- Filling the pool in a test: the fixture `test/track-select` (`tests/fixtures/modules/track-select`) calls the pool API for the
  first table in the image (handle 1). Request 0x3e with body `[1, 0x61, ...path bytes]` is `sample_pool_add` (the reply body is the pool index);
  `[1, 0x63, index]` is `sample_pool_clear`; request 0x3f with body `[1, index]` returns the BE32 status (3 = ready) and, when ready, frames,
  rate, channels, token.

- `src/module.c`: the machine, parameters, page and the pool table declaration; `dkm_init` resolves the table handle and starts the picker;
  `shot_smp_click` (the SMP click action) opens the pool list with the current entry marked; `smp_pick` sets SMP.
- `src/dsp/shot.s`: `render`, `note_on`, the table storage and the descriptor. Every sample, mono or stereo, is read by the SDK stereo kernel
  `_dk_sample_pcm16_stereo_linear` (a mono sample passes `base_r = base_l`); there is no mono path.
- `tests/test_shot_playback.py`: the DSP on the concrete core through the real adapter, against a float32 model (SMP picks the record).
- `tests/shot-dt2.test.ts`, `tests/shot-dn2.test.ts`: the pool API, the stereo output and an SMP phase on the emulator.

```sh
mise run module:check -- digikit/shot
```
