# SHOT

digikit/shot: a stereo sample-playback machine for the Digitakt II and the Digitone II, loaded at run time. It plays one entry of its sample pool
once per note (a one-shot) through the per-module pool of API 1.5 (`<dk/sample_track.h>`): 16 entries, kept per project. The parameter SMP
picks the entry. The picker adds PCM16 files (mono or stereo) to the pool through the runtime and sets SMP to the chosen entry; SHOT reads the pool.

| Word | Parameter | Knob | Range and default | Meaning |
| --- | --- | --- | --- | --- |
| 0 | SMP | D | 0 to 15, default 0 | pool entry that plays; turning steps through the pool, p-lockable; an empty entry is silent |
| 1 | START | E | 0 to 127, default 0 | first frame, spread over the whole sample: frame = round(frames * v / 127) |
| 2 | END | H | 0 to 127, default 127 | end frame (exclusive) the same way; END at or below START plays nothing |
| 3 | PLAY | B | 0 to 3, default 3 | 0 REV, 1 REV.L, 2 FWD.L, 3 FWD (the stock PLAY modes); .L loops the region START to END |
| 4 | TUNE | A | 0 to 127, default 64 | 64 is the played note unchanged; each unit is 3/8 semitone, so -24 to +23.6 semitones |

Both devices use the same two opt-in pages (machine contract 1.5). The control page binds TUNE A, PLAY B, SMP D, START E and END H,
with the native left LEV fader. A bounded overlay uses code-native tune/sample boxes and a portable PLAY mark. TUNE displays signed whole
semitones and an exact fractional row, without changing pitch's 0..127 range or 3/8-semitone scale. An empty selected pool slot is a dotted
sample box at D; a ready box shows its local index, with compact loading/missing marks. There is no center status text. START is bottom-left E and END
bottom-right H on both pages. The waveform page has ZOOM at G. One detent is one step; 8 steps double the zoom (ratio 2^(steps/8)), up to one frame per pixel
(at most 16 doublings); one turn event moves at most 16 steps. The label is `x1.0` to `x9.9` with one decimal, `x10` and up without, `xMAX` above 9999. Zoom is UI-local per track.
It shows the loaded sample's min/max overview, a permanent zero line and contrasting trim flags. Zoom is anchored on the active marker: START each time page 2 is entered, then the marker turned last on page 2 (START wins when both changed). Its value is underlined. When the zoom
changes, that marker keeps its screen position, and 1x shows the whole sample. Turning START or END pans the view only when the marker is outside it,
just far enough to put it 1/8 of the span from the edge it came from. A marker outside the view is drawn as a small solid arrow at that side of the plot
(at its flag row) instead of a line. The waveform is scaled vertically to the visible peak, up to 8x (display only; silence stays flat). When one pixel covers less than one overview bin, the page asks the runtime for a detail
window (`sample_pool_peaks_window`, 95 columns of the viewport, computed in the background); until it is ready the previous detail window stays on screen (rescaled) where it covers the viewport, and the overview is drawn stretched elsewhere. The
ready sample box shows its local slot index; the waveform page keeps its upper area free for the graph. The graph spans global x17..111 and y6..36, centered at x64; START, ZOOM and END are drawn on page 1's label row (canvas y52, x5/58/87, the same pixels as the stock labels), with their values one row above (y46), each centred on its label using a measured font width table. SRC/SYN cycles the two pages.
The ordinary `DK_PAGE` remains the storage/parameter schema; its fixed knob slots do not constrain these custom bindings.

- Pool: click SMP at D (release without a turn) to open entries 00 to 15 with name and state. The current entry is marked with `>` and selected.
  YES on an empty or filled slot opens its file browser; YES on a file loads that slot, sets SMP and returns to the control page. Slots retain
  their indexes when cleared. RIGHT on an entry gives CLEAR and INFO; NO goes back one level. The pick writes SMP of the clicked track with
  `param_set` (API 1.6); while TRIG or FUNC is held it writes nothing and the page shows `RELEASE TRIG TO SET SMP`. The chooser is described in
  `docs/design/ui-rules.md`. `DK_PARAM_ACTION(shot, 0, shot_smp_click)` follows the custom D binding; the test route `test/track-select` fills the
  pool in tests.
- Pitch: MIDI note 60 plays the sample at its own speed; the cursor rate is `srcrate / 48000 * 2^(semitones / 12)` in Q16.16, limited to 1/65536 to 64
  frames per sample. As in the stock one-shot, SMP is read at note-on; START, END, PLAY, PITCH and the note follow every block. A PLAY
  change that flips the direction does so at the current cursor. FWD reads from the cursor to END (a later START has no effect); REV reads
  down to START. An END moved behind the cursor (or START above it in REV) ends the voice in that block with the end ramp.
  REV.L and FWD.L loop the region [START, END) in the direction of play: the cursor starts at END - 1 (REV.L) or START (FWD.L) and wraps by the
  region length at the other end, so the loop is seamless. A looping voice never ends by itself (the AMP envelope shapes it; a new trigger
  restarts it). A region of fewer than 2 frames does not loop and plays nothing. In a loop mode a cursor outside the region (START or END moved)
  goes to START (FWD.L) or END - 1 (REV.L). A rate above the region length in frames per sample ends the voice (one wrap per frame).
- Stereo: a stereo sample gives distinct L and R through machine contract 1.2 (`out_r`). A mono sample plays centred (`base_r = base_l`). On the
  Digitakt II R goes to the odd voice of the stock voice pair. On the Digitone II L and R are both heard: all pre-pan stages have R twins
  (`docs/design/dn2-stereo.md`, phase 2); the fallback to R = L is only a guard.
- Lifecycle: `note_on` takes the pool index from the SMP word and copies that entry's `dk_strack` record under the admission and epoch macros; every
  `render` checks the epoch and acquires and releases the entry. A stale token (clear, a new load, a stock reset) or an entry that is not ready gives
  a silent block and `DK_DONE`.
- Packing: `patchkit pack digikit/shot --device dt2|dn2` carries the pool table in the DSP section (kind 2 record) and declares API 1.5 and machine
  contract 1.5. The runtime reserves it when the module loads and writes the records over the DSP link once the DSP image is in.
- Filling the pool in a test: the fixture `test/track-select` (`tests/fixtures/modules/track-select`) calls the pool API for the
  first table in the image (handle 1). Request 0x3e with body `[1, 0x61, ...path bytes]` is `sample_pool_add` (the reply body is the pool index);
  `[1, 0x63, index]` is `sample_pool_clear`; request 0x3f with body `[1, index]` returns the BE32 status (3 = ready) and, when ready, frames,
  rate, channels, token.

## Sources

- `src/module.c`: the machine, parameters, page and the pool table declarations; `dkm_init`/`fini` resolve the table handle and start the picker;
  `shot_smp_click` (the SMP click action) opens the pool list with the current entry marked; `smp_pick` sets SMP.
- `src/pages.c`: the two native pages (draw and event callbacks). The short name and page name are SHT.
- `src/sample_view.c`, `src/sample_view.h`: the view of one pool slot (info, peak copy and the detail window cache).
- `src/shot.h`: internal header shared by the C sources.
- `src/shot_params.h`: parameter word indexes shared by C and the DSP assembly.
- `src/dsp/shot.s`: `render`, `note_on`, the table storage and the descriptor. Every sample, mono or stereo, is read by the SDK stereo kernel
  `_dk_sample_pcm16_stereo_linear` (a mono sample passes `base_r = base_l`); there is no mono path.

## Tests


- `tests/pages.test.ts`: C host test of the pages and the pool view (`tests/pages-host.c`). The SDK widget and region tests live in `sdk/tests` (run by `sdk:test-host`).
- `tests/test_shot_playback.py`: the DSP on the concrete core through the real adapter, against a float32 model (SMP picks the record).
- `tests/shot-dt2.device.test.ts`, `tests/shot-dn2.device.test.ts`: the pool API, the stereo output and an SMP phase on the emulator.

```sh
mise run module:check -- digikit/shot
```
