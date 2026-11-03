# 109. DT2 1.17: a module can borrow the stock sample pool

Trigger: the sampler needs PCM backing on DT2. This finding checks whether a module can place its own samples in the stock sample pool. Static results are read once from the DT2 1.17 image. Emulator results are marked separately. Marks: **[D]** measured or read once, **[O]** open.

## 1. Tables and records **[D]**

| Table | Address | Content |
| --- | --- | --- |
| Slot-to-record map | `0x405b5368[slot]` | int32 record index |
| Key | `0x405b1368 + slot*0x10` | 16-byte key per slot |
| Name | `0x405b6368 + slot*0x10` | name per slot |
| Stereo flag | `0x405ba368[slot]` | byte |
| Length, header, name pointer | `0x405ba768`, `0x405bb768`, `0x405bc768` | per slot |

Record vector `0x405a0f64..0x405a0f68`, stride 0x34:

| Offset | Content |
| --- | --- |
| `+0x10` | pool block offset |
| `+0x14` | rounded size |
| `+0x18` | gap before the block |
| `+0x20..0x30` | set of sharing slot ids |
| `+0x30` | count of sharing slots |

DSP slot record: `0x257810 + slot*0x14`, written by `0x1c3fe5`. Word 0 minus 0x40 equals the block offset. PCM byte address is `0x8422b9c8 + offset`.

## 2. Loader, release and reset **[D]**

Loader `0x4015481c(keyptr, slot)`:

- Requires slot <= 0x3ff and `(slot & 0x7f) != 0`.
- Releases the slot's old block first (`0x401548f4`).
- Deduplicates by key: `0x4015b616` is the same-slot check, `0x4015389e` finds a shared record.
- The append fails above `0x19000400` and returns -1 ("SAMPLE POOL FULL"). Success returns 0.

Release `0x401543a2` shifts only the 0x34-byte records and rebuilds the map (`0x4015394a`). PCM never moves. A block's offset is fixed for its life.

Slot ids are bank<<7 | index (8 banks of 127; index 0 means none). Stock uses ids 1..0x3ff (first free search `FUN_40016702`). Preview uses the fixed slot 0x400. No id is reserved for modules.

Persistence: only the stock assign wrapper `0x4004b502` writes the key into the project settings (`settings + 2 + (slot+0x1d)*0x10`). A direct loader call writes RAM tables only.

Reset `0x40154634` clears slots 0..0x3ff. It is called only from `ProjectSettings::ReloadAllSamples` (`0x4004e8be`) with arg 1. Callers with 1:

| Caller | Path |
| --- | --- |
| `0x400110ca`, `0x4001190a`, `0x40011b40` | card repair confirm windows |
| `0x40030d04` | boot load all samples |
| `0x40030fe2` | kit or preset received over SysEx |
| `0x400461a6` | project new **[O]** (owner not checked) |
| `0x400461cc` | `loadProjectFromMemory` |
| `0x400461f2` | `loadProjectFromMmc` (project load) |

`0x400a0ad8` passes 0. Purge `FUN_40043d9a` loops over slots 1..0x3ff and removes only slots with a project entry.

## 3. Emulator results **[D]**

Tests: `tests/regression/runtime/stock-sampler-lifetime.test.ts` (case `unloadEarlierWhilePlaying`) and `tests/regression/runtime/stock-pool-borrow.test.ts` with the module `tests/fixtures/modules/stock-pool`. Raw values are in `out/runs/stock-pool-borrow/` and `out/runs/stock-sampler-lifetime/`.

- Unloading slot 1 while slot 2 played: slot 2's record index went 4 to 3. `+0x10` stayed 1262592 and `+0x14` stayed 966656. The DSP record and the first 64 PCM bytes were unchanged. The voice kept playing.
- Direct load into slot 0x3ff with a stock slot's key: rc 0. It shares the stock record (sharers 5 to 6), with the same DSP record and PCM.
- File load into slot 0x3fe was queued on SampleLoaderBgWorker (getter `0x4019e65c`, queue `0x400f0c1c`). Offset 295936. The PCM matches the file.
- Slot 0x100 returns -1.
- Project save with our slots loaded: the slot-area sectors did not change. The card overlay is cumulative, so this is weaker than a diff against a separate file. The save UI ran each time.
- Purge leaves our slots loaded. Unexplained: the stock-assigned slots 0x81, 0x90 and 0x101 also survived the purge **[O]**.
- `ReloadAllSamples(..., 1)`: our high slots clear last, about 1.5 s after the call. After the reset the slot map reads 0 (not -1; only release writes -1). Key word 0 is 0xffffffff. The DSP record is `fffffc00 fffffc00 0 0 0`. Detect loss by key word 0 or by the DSP record length, not by the map.
- The emulator is single-threaded. The loader's locking on the real device is **[O]**.

## 4. Conclusion

Borrowing is feasible with these conditions:

- Call the loader directly. Never call the assign wrapper.
- Take slots from 0x3ff down.
- Drain own readers on a slot before reloading it.
- Detect a stock reset and reload.
- Handle -1.
- Run loads on SampleLoaderBgWorker.

Device-specific code lives in an adapter (`core/adapters/sample-pool`) with anchors in `devices/fw`, not in `core/runtime`. Content modules see only the device-neutral API.
