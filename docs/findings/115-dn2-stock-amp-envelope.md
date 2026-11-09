# 115. DN2: the stock amp envelope (tables, phases, modes, times)

Trigger: a ColdFire-side LFO 4 needs the time at which a stock voice's amp envelope ends. Scope: DN2 1.12 section 7 (SHA-256 `336e340a…`, the digikit pin). DN2 1.11 has the same section 7, byte for byte. Marks: **[D]** measured or read once, **[O]** open.

## 1. The setter **[D]**

`0xb8028c` sets one voice's envelope record. The engine calls it at `0x1c92b0`, with the unpacked controls at `DM(I5-7)` to `DM(I5-1)` (`I5` = track record `+0x228`).

- `R8` = `DM(I5-7)`: the mode, an integer.
- `R12` = attack. The stack arguments: hold, decay, sustain, release, reset.
- Attack, hold, decay and release become a table index: `index = trunc(clamp(127 * x, 0, 127))`.
- Sustain = `clamp(x, 0, 1)`, kept as a float.
- In mode 1 the setter sets sustain to 0 and the release index to the decay index.

The record words:

| word | contents |
| --- | --- |
| 0 | attack index |
| 1 | hold index |
| 2 | decay index |
| 3 | sustain (float) |
| 4 | release index |
| 5 | reset flag |
| 6 | mode |
| 7 | phase |
| 9 | hold counter |
| 10 | gain: 0 is silent, -1.0 is full level |

## 2. The tables **[D]**

Three 128-word tables in section 7. The loader places them at `0x28000000` + the normal-word address.

| table | NW address | type | used for |
| --- | --- | --- | --- |
| A | `0x26a570` | float, negative | attack step per sample. `A[0]` = -1.0 |
| B | `0x26a770` | float, negative | decay and release: the factor `c = -B[i]`. `B[127]` = -1.0 |
| C | `0x26a970` | integer | hold length in samples: 2, 14, 40, 82 … `C[126]` = 470,679, `C[127]` = 480,000 |

All three tables are monotonic.

## 3. The stage **[D]**

`0xb80345` runs one voice for one block of 32 samples (48 kHz). Its arguments: the control word (engine `+0x1390c + v`), the note-off byte (engine `+0x1391c + v`) and 32.

| phase | segment | rule |
| --- | --- | --- |
| 2 | attack | linear. The gain starts at `0.5 * A[i]` and adds `A[i]` per sample until it is at or below -1.0 |
| 3 | hold | `C[i]` samples, in mode 1 only. Index 127 holds until note-off (read from the code, not run) |
| 1 | decay to sustain | `h' = c * h + S * (1 - c)` |
| 4 | decay or release to 0 | `h' = c * h` |
| 5 | steal fade | the gain falls to 0 in one block and stays 0 |

- `c = -B[i]`. At index 127, `c` = 1.0, so a decay or release at 127 never falls.
- The stage has no end state. Phases 1 and 4 are exponential. The gain word reaches exact zero only by float underflow (2^-126).
- With the reset flag at 0, a retrigger keeps the current gain. The attack is then shorter than its table time.

## 4. The modes **[D]**

| DSP mode | sequence | note-off |
| --- | --- | --- |
| 1 (AHD) | attack, hold, decay to 0 | acts only when HOLD is 127 |
| 2 (ADSR) | attack, decay to sustain | starts the release. HOLD is not read |
| any other word, 0 included | attack, decay to sustain | ignored |

On the ColdFire, AMP MODE is parameter 91: range 0 to `0x0100`, default `0x0100` (the 1.11 parameter table). The formatter `0x400e3482` shows "AHD" when `value >> 8` is 0 and "ADSR" for any other value (read from the code, not run).

The frame unpack (`0x1c2712`) maps parameter 91 through a two-word table at index `value >> 8`. Measured in digikit's native engine, from a post-init snapshot, for machine types 0 to 4 (record `+0x20c`):

| parameter 91 | DSP mode |
| --- | --- |
| `0x0000` (screen: AHD) | 1 |
| `0x0080` | 1 |
| `0x0100` (screen: ADSR, the default) | 2 |
| `0x0200` | 1 |
| `0x7f00` | 2,471,144 |
| `0xffff` | 2,472,680 |

- A control showed that the unpack ran: SUS `0x4040` arrived at `+0x21c` as `0x3f018306` (`0x4040 / 32512`).
- A value of `0x0200` or more reads past the table. `0x0200` happens to read 1. The high values read words that look like addresses.
- The engine init (`0x1c8e67` → `0x1c7069`) sets `+0x20c` to 0 on all 16 voice records once. The first unpack overwrites it.
- The SHARC database shows no other direct store to `+0x20c` **[O]**: a store through a computed pointer is not excluded.

## 5. Times **[D]**

A float32 model of each segment, sample by sample, from the tables. Index = value. Decay and release run from full level.

| value | attack s | hold s | decay or release to -60 dB, s | to -96 dB, s | to zero (underflow), s |
| --- | --- | --- | --- | --- | --- |
| 0 | 1 sample at most | 0.00004 | 0.00015 | 0.00025 | 0.0019 |
| 1 | 0.00006 | 0.00029 | 0.0009 | 0.0014 | 0.011 |
| 10 | 0.0031 | 0.0216 | 0.065 | 0.104 | 0.82 |
| 32 | 0.048 | 0.337 | 1.01 | 1.62 | 12.8 |
| 64 | 0.262 | 1.84 | 5.51 | 8.82 | 69.7 |
| 96 | 0.714 | 5.00 | 15.0 | 24.0 | 189.6 |
| 126 | 26.9 | 9.81 | 29.4 | 47.1 | 372.3 |
| 127 | 29.6 | until note-off | never | never | never |

Check on digikit's rig: the real stage `0xb80345`, called through `tools/patchkit/pk_sharc` `CallSession` on the section 7 image, gave the model's sample counts:

- attack 10: 149 samples
- hold 2: 40 samples
- decay or release 10 to -96 dB: 4,988 samples
- release 127: the gain stays at full level

## 6. Open **[O]**

- The index rule assumes the unpack divides by 32512. If it multiplies by a rounded `1/32512`, values 9, 13, 18, 26, 36, 52, 72 and 104 get one index less. The times are monotonic, so index = value is an upper bound in both cases.
- The mode mapping ran on one engine only. The Python runner and a unit did not check it.
- No time in section 5 was measured on a unit.
