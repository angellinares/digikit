# Project and kit storage on DT2 1.17

Digitakt II OS 1.17 only. Marks: **[D]** read or measured once, **[O]** open. Nothing here is **[V]**. Rows come from a
patched-build session (closed PR #215); the addresses and call sites were re-read in the 1.17 database on 2026-11-11.
Already covered, not repeated here:
- `Project*` global, getters, setter, save and load wrappers: [digikit-project plan](../plans/digikit-project.md), "Firmware facts".
- File magic, version 5, tail offsets, sizes: [finding 110](110-project-identity.md) and [finding 02](02-machines-and-parameters.md) (native project format).
- Card slots: [finding 14](14-plus-drive-format.md). RAM tables on 1.16: [finding 09](09-runtime-state.md).

## 1. The project in RAM **[D]**

| Part | DT2 1.17 |
| --- | --- |
| Project object (the autosave passes it to `0x400df66c`) | `0x41776c9c` |
| Patterns: object + `0x1c`, 128 records | `0x41776cb8`, stride `0x1db8c` |
| Kits: object + `0xedc61c`, 128 records | `0x426532b8`, stride `0x5574` |

- Each pattern has its own kit: kit n is pattern n's. A separate kit library exists too (KIT MANAGER).
- The pattern that is on: `*(*0x44f370a0 + 0x138)` (the `PATTERN_OBJ` pointer that getter `0x40198b7e` returns). It was
  the only word that went 0, 1, 2 as A01, A02, A03 came on. A write guard reported one writer, `0x401a239c`.
- A kit record has no spare byte: all `0x5574` are serialized.
- A pattern record's bytes + `0x1cef0` .. `0x1db60` are not saved but in use: `0xff`, and a 16 x 128 array read by the code at
  `0x400d6cd6`. Pokes there hung the UI.

## 2. The routines **[D]**

`0x400df66c` is the live-project-to-file serializer, not only a header copy. Decompiled:
`(file, project, progress_arg, flags, progress)` and the `flags` bits pick the parts:

| Flag | What it writes |
| --- | --- |
| `0x20` | 16-byte name, project + `0xc` to file + 8 |
| `0x80` | stamp: file word 6 (+ 24) = project + 8 |
| `0x01` | one block at file + `0xdc8600` (`0x400dd76e`; what it holds is not read) |
| `0x04` | 128 patterns with `0x400dc894`, file + 512, record `0x15c00` |
| `0x02` | 128 kits with `0x400dbdca`, record `0x5800` |
| `0x08`, `0x10` | two more blocks (`0x400dcc4e`, `0x400dbd7c`) |
| `0x40` | head magic `0xbeefbace`, version 5, tail magic |

So the plan's "live to card header copy" (flag `0x80` of this routine, name and stamp) and the PR's "save routine" both
name this function; the whole routine is the serializer. The load twin is `0x400df95a` (`write_live_project`): checks
magic, tail and version 5, then name, stamp, settings, 128 patterns, kits.

| | DT2 1.17 |
| --- | --- |
| Pattern serializer / loader | `0x400dc894` / `0x400dc702` |
| Kit serializer / loader | `0x400dbdca` / `0x400db898` |
| Version converters in the pattern loader (from 3, from 2) | `0x400de220`, `0x400de420` |

Pattern record: version 4, 16 tracks x `0x4a0` (RAM `i * 0x6a5`), `0xc000` (RAM + `0x6a50`), `80 x 258` (RAM + `0x17e50`),
raw tail of `0x28` bytes from RAM + `0x1db60`, ending at `0x15acc`. The tail is copied to record + `0x15aa4` by the memcpy at
`0x400dc8cc`; the converters show it moved from `0x15a94` to `0x15aa4` between versions.
Kit record: head magic, version 4, fields, tail magic at + `0x573a`.

Bytes no serializer writes (zero in every save seen):

| Where | Range |
| --- | --- |
| Pattern record | `0x15acc` .. `0x15c00` (308 bytes) |
| Kit record | `0x573e` .. `0x5800` (194) |
| File header | `0x1c` .. `0x200` (484): only + 0, 4, 8, 24 are touched |

## 3. Hook sites **[D]**

Each is one whole 6-byte instruction. Re-read now: kit save `0x400dbee4` (`move.l #0xbacef00c,d1`, before the tail store),
kit load `0x400db8be` (same instruction, after the version check), header save `0x400df932`, header load `0x400df99c`,
pattern raw-tail memcpy `0x400dc8cc` (`jsr`), pattern load `0x400dc7e6` (`move.l #0x1db88,d0`).

| Site | DT2 | Registers (from the session) |
| --- | --- | --- |
| Kit save | `0x400dbee4` | a2 record, a3 kit RAM |
| Kit load | `0x400db8be` | a2 kit RAM, a3 record |
| Header save | `0x400df932` | a3 file; d0 live after it |
| Header load | `0x400df99c` | a3 file |
| Pattern save (raw tail) | `0x400dc8cc` | d2 record, d3 pattern |
| Pattern load | `0x400dc7e6` | a2 pattern, d2 record + `0x10a04` |

In a save each kit passes the serializer twice, in a load the loader twice: a counting hook sees two.

Callers (database `callers`, 2026-11-11):
- Kit serializer, besides `0x400df66c` (call `0x400df83c`): `0x40035532`, `0x40035790`, `0x40043834`, `0x400d3bae`,
  `0x400e2ee8`, `0x400e3034`, `0x401332b8`.
- Kit loader, besides `0x400df95a` (call `0x400dfacc`): `0x40044612`, `0x400485be`, `0x40048978`, `0x400dfde6`, `0x400e1570`.
- The session listed `0x40035292`, `0x40035760` (serializer) and no `0x400dfde6` (loader). The database does not show
  those two, and the session's counts (9 and 4) differ from it (7 and 5) **[O]**.
- `0x40044612` loads kit n of the project object from a copy at `0x409908e4` (the reload); `0x40043834` saves a table kit to
  `0x40996ce4`. The others were not traced **[O]**.

## 4. Save buffer **[D]**

The buffer at `0x4099d698` holds the file loaded at boot and ends where the project object begins. SAVE PROJECT AS right
after boot does not write from it (poked bytes did not reach the card; the new file has zeros in the unused ranges). A save
after another save serializes into the buffer the loaded file is in, so bytes no serializer writes keep what the loaded
file had. Stock files alone: a project with non-zero unused bytes, loaded and saved as another slot, kept them.
A stock save, load and save changes only the header word at + 24.

## 5. Kit copies **[D]**

Pattern copy and paste (FUNC + RECORD, FUNC + STOP) copy the kit with memcpy `0x40136388` of the kit size. Seven calls push
that size as length: `0x40035e3e`, `0x40035ee0`, `0x40036c30`, `0x40133412`, `0x401a900a`, `0x401a9080`, `0x401cdcdc`
(not re-read). memcpy starts with 12 bytes of whole instructions: `movea.l 4(sp),a1; movea.l 8(sp),a0; move.l 12(sp),d0`.
A hook there saw the paste of A01 onto A04 reach A04's kit through kit-sized memcpy calls only, directly or through a
buffer filled by an earlier kit-sized copy. PASTE KIT and undo were not driven **[O]**.

## Open **[O]**

- Which kit manager action each serializer and loader caller is, and whether all pass table kits.
- Whether any kit copy avoids memcpy (a loop, part by part).
- All of this on DN2 1.12 (addresses and layout differ; PR #215 held values that are not re-read here).
