# PARAM_CHANGE: the selected track's Sound words, read through anchors (2026-11-10)

Question: can `digikit/debug` log a change of a Sound parameter without a fixed address, so a test waits for "the firmware stored the value" instead of a fixed time?

Answer: yes, for the Sound array of the selected track, with one new function anchor (`settings`) and an existing one (`native_track_sound`, extended to DT2) and plain reads. `digikit/debug` polls the words once per UI pass and logs `PARAM_CHANGE` (kind 42, debug level 3: track, index, old, new). No firmware hook, no write. The second check against the image bytes (a second agent, DT2 1.17 and DN2 1.12) is done for the items marked **[V]**.

## Path [V]

```text
p      = cf.project.fn.singleton()
sel    = cf.page.fn.view_track(cf.project.fn.settings(p))          ; the selected track, clamped to 0..15
sound  = cf.page.fn.native_track_sound(cf.project.fn.active_kit(p), sel)
record = sound->vtable[0x28](sound)                                ; *(sound + 0x10); 0 when no record is bound
word i = s16 big endian at record + 0x14 + 2 * i
```

| Item | DT2 1.17 | DN2 1.12 |
| --- | --- | --- |
| `cf.project.fn.settings` (`move.l 4(SP),D0; addi.l #0x30,D0; rts`) | 0x40041d18 | 0x4003e40e |
| `cf.page.fn.native_track_sound` (clamp 0..15, kit + 0x6c + 0x3c0 * track) | 0x40035cca | 0x40030e66 |
| Words in the array (`[fx] sound_words` of the device file) | 71 | 101 |

Both anchors are unique by hash on both devices (`settings` in `devices/fw/project.toml`; `native_track_sound` in `devices/fw/page.toml`, used by core/machines on DN2 and by core/runtime on both). Ports: `devices/ports/{dt2-1.17,dn2-1.12,dn2-1.11}.toml`.
`track`, `kit` and `project` use the existing anchors `view_track`, `active_kit` and `singleton`. The record of a track is at an odd address on DN2 (stride 0x48b in the kit blob), so the runtime reads the words byte by byte.

**Code to index [V].** A parameter code maps to its word through the descriptor table (anchor `cf.param.data.descriptor_table`): row = base + code * 0x3c, the mirror index is the s32 at row + 4, -1 means the code has no word in this array. The index is a position: the same index has other codes on other machines (finding 02). The log therefore carries the index; a host maps it to a code only for display.

## The runtime import and the log

`dkrt_selected_params(i16 *out, u32 n)` (sdk/sdk.toml) writes the first `n` words of the selected track's array to `out` (words past the device's count are 0) and returns the track, 0 to 15; -2 when there is no project, -3 when the track has no record (`out` is not written). One call gives one consistent snapshot of one track and needs no second import for the track number. It is read-only, so a stale pointer cannot do harm beyond a wrong value.

`digikit/debug` keeps the last snapshot (101 words) and, once the track watch has started (3000 blocks after the first pass, as for MACHINE_CHANGE), compares it once per UI pass. A changed word logs `PARAM_CHANGE`. A change of the selected track, or a pass that returns -3, takes the snapshot again silently, and every snapshot logs one baseline event (PARAM_CHANGE with index 0xffffffff, old and new 0) so a host can wait on firmware state before it turns a knob (a track switch would otherwise log up to 71 events that are no edits). Host test: `modules/tools/debug/tests/test_debug_module.py`.

## Marks

- **[V]** The two anchors and their bytes, `view_track` (settings + 8 through vtable slot 0x28 of the settings object, 0 on a null pointer), the vtable slot 0x28 of the Sound value object (`*(object + 0x10)`), the words at +0x14, the counts 71 and 101, and the code-to-index table.
- **[D]** ProjectSettings is at project + 0x30 because the Project constructor builds it there; only the `settings` function's body shows the offset directly.
- **[D]** The array is the one the stock getter and setter use, and the one the DSP words are built from (finding 04). Writes that do not go through the Sound array do not show: p-lock, sound lock, LFO and MIDI CC use other paths. No write path was listed completely.
- **[D]** The record pointer can be 0 or stale while a kit loads. The runtime checks for 0 and the log re-snapshots; a stale non-zero pointer during a load was not observed and not excluded.
- **[D]** A knob turn on the selected track shows: `debug-module.device.test.ts` turns knob A of the main screen on both devices and gets a PARAM_CHANGE with the new value above the old. The test waits for the baseline event first: a turn before the first snapshot is not logged (a fixed 3 s wait failed on both devices).
- **[D]** DN2: the record of track 0 is `*0x800052a0 + 0x34` (read once through the import; the same record the fixed-address route reads).
- **[D]** **SMP of the SHOT machine does not appear in this array.** On DN2, ten remote turns of SMP gave no PARAM_CHANGE, and a dump of all 16 tracks' 102 halfwords before and after three turns showed no changed word, with TRIG 1 held (the `setSmp` case) and without it. SMP is a latched parameter (`DK_PARAM_LATCHED`) of a run-time machine, a stock parameter row that the Sound array does not hold; where its turn is stored was not found. So `setSmp` cannot wait on PARAM_CHANGE of an SMP word and keeps INPUT_TAKEN and the DSP word. DT2 was not tried. **[O]** the store of a latched run-time parameter.
