# SHOT

digikit/shot: a stereo sample-playback machine for the Digitakt II and the Digitone II, loaded at run time. It plays one entry of its sample pool
once per note (a one-shot) through the per-module pool of API 1.5 (`<dk/sample_track.h>`): 16 entries, kept per project. The parameter SMP
picks the entry. SHOT never selects a file: the picker adds PCM16 files (mono or stereo) to the pool through the runtime, SHOT reads them.

| Word | Parameter | Knob | Range and default | Meaning |
| --- | --- | --- | --- | --- |
| 0 | SMP | B | 0 to 15, default 0 | pool entry that plays; turning steps through the pool, p-lockable; an empty entry is silent |
| 1 | START | C | 0 to 127, default 0 | first frame, spread over the whole sample: frame = round(frames * v / 127) |
| 2 | END | E | 0 to 127, default 127 | end frame (exclusive) the same way; END at or below START plays nothing |
| 3 | REVERSE | F | 0 to 127, default 0 | 64 and above plays backwards from END - 1 to START |
| 4 | PITCH | G | 0 to 127, default 64 | 64 is the played note unchanged; each unit is 3/8 semitone, so -24 to +23.6 semitones |

The knob letters are the same on both devices (the shared knob map of `param_slots`: words 0 to 4 on B C E F G). The Digitakt II keeps TUNE on
A, the sample slot on D and LEV on H; the Digitone II has A, D and H free.

- Pool: click SMP (release without a turn) to open the list: `+ ADD`, then the entries 00 to 15 with name and length. YES on `+ ADD` opens the
  folder browser; YES on a file adds it and selects it; RIGHT on an entry gives CLEAR and INFO; NO goes back to the page. The page hook and
  `shot_smp_action` are described in `src/module.c` and in `docs/design/sample-picker.md`. Until the hook exists the pool is filled through the
  test route `test/track-select`.
- Pitch: MIDI note 60 plays the sample at its own speed; the cursor rate is `srcrate / 48000 * 2^(semitones / 12)` in Q16.16, limited to 1/65536 to 64
  frames per sample. SMP, START, END and REVERSE are read at note-on; PITCH and the note follow every block.
- Stereo: a stereo sample gives distinct L and R through machine contract 1.2 (`out_r`). A mono sample plays centred (`base_r = base_l`). On the
  Digitakt II R goes to the odd voice of the stock voice pair. On the Digitone II only L is heard until the shadow right-channel stages are
  production (adapter TODO).
- Lifecycle: `note_on` takes the pool index from the SMP word and copies that entry's `dk_strack` record under the admission and epoch macros; every
  `render` checks the epoch and acquires and releases the entry. A stale token (clear, a new load, a stock reset) or an entry that is not ready gives
  a silent block and `DK_DONE`.
- Packing: `patchkit pack digikit/shot --device dt2|dn2` carries the pool table in the DSP section (kind 2 record) and declares API 1.5 and machine
  contract 1.2. The runtime reserves it when the module loads and writes the records over the DSP link once the DSP image is in.
- Filling the pool in a test (no click hook yet): the fixture `test/track-select` (`tests/fixtures/modules/track-select`) calls the pool API for the
  first table in the image (handle 1). Request 0x3e with body `[1, 0x61, ...path bytes]` is `sample_pool_add` (the reply body is the pool index);
  `[1, 0x63, index]` is `sample_pool_clear`; request 0x3f with body `[1, index]` returns the BE32 status (3 = ready) and, when ready, frames,
  rate, channels, token.

- `src/module.c`: the machine, parameters, page and the pool table declaration; `dkm_init` resolves the table handle and starts the picker;
  `shot_smp_action` opens the pool list.
- `src/dsp/shot.s`: `render`, `note_on`, the table storage and the descriptor. Every sample, mono or stereo, is read by the SDK stereo kernel
  `_dk_sample_pcm16_stereo_linear` (a mono sample passes `base_r = base_l`); there is no mono path.
- `tests/test_shot_playback.py`: the DSP on the concrete core through the real adapter, against a float32 model (SMP picks the record).
- `tests/shot-dt2.test.ts`, `tests/shot-dn2.test.ts`: the pool API, the stereo output and an SMP phase on the emulator.

```sh
mise run module:check -- digikit/shot
```
