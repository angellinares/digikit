# Machines and parameters

The ColdFire machine dispatch: the descriptor and parameter tables, display names, the type-7/PLACEHOLDER clone work, the permission check, the SRC slot model and CFADE.

## The ColdFire machine dispatch **[V]**

Found by emulator read-watch, not statically. Earlier analysis had concluded
the descriptor table was write-only — every entry had exactly one reference, a
write from the static initialiser `FUN_401ac1be`, and no readers anywhere. That
was correct as far as it went: the table is uninitialised bss, so it exists
only at runtime, and Ghidra throws `MemoryAccessException` reading it. Running
`tools/mmiotrace.py` range-scoped over it on a boot resumed from
`snapshots/boot400M.snap` gave 432 reads, all from a single PC, `0x4001767a`. **[C]**

The dispatch is 34 bytes at `0x400caf48`:

```
400caf48  moveq  #6,D1               ; the bound -- one byte
400caf4a  move.l (4,A7),D0           ; machine_type
400caf4e  cmp.l  D0,D1
400caf50  bcs.b  $400caf62           ; type > 6 -> fallback
400caf52  move.b #0x2c,D1            ; stride, 44 bytes
400caf56  mulu.l D1,D0
400caf5a  addi.l #0x42923644,D0      ; descriptor array base
400caf60  rts
400caf62  move.l #0x4292374c,D0      ; == base + 6*0x2c, i.e. entry 6
400caf68  rts
```

So an out-of-range machine type resolves to MANUAL SLICE rather than crashing —
a forgiving failure mode for anything that patches this. The field accessor is
`FUN_4001762c(obj, field)` → `*(descriptor + 8 + field*4)`; `FUN_400caf48` has
six callers, all resolvable. Each descriptor is two string pointers, nine
literal ID fields and a trailing tag of 10.

Dumped live with `tools/memdump.py` — the only way to see it, since it is bss —
the seven entries are the machine list in order: `0 SAMPLE`/`SAMP`, `1 WERP`,
`2 STRETCH`, `3 REPITCH`, `4 SLICED SMP`/`SLIC`, `5 MIDI`, `6 MANUAL SLICE`/`MLIC`.
Entry 5 (MIDI) is the one irregular record — all nine ID fields zero and no
tag, which shows the IDs are not mandatory. Entries 3 and 6 carry six IDs
rather than seven. **[V]**

Those are **not** the names the UI shows — they look like internal or legacy
labels. See "The display names are a separate table" below. **[C]**

The UI's list length is not a numeral. `MachineSelectionView` (`FUN_400607b2`)
builds two `std::vector<int>` by copying a rodata range, so the count is a pair
of pointer immediates; `MachineListView` (`FUN_4005e022`) enumerates nothing
and renders one row per vector entry. **[V]**

| list | table | contents | built by |
|---|---|---|---|
| source | `0x401e1958`-`0x401e1974` | `{0,1,2,3,6,4,5}` — 7, in UI display order | `FUN_40051fbc` |
| filter | `0x401e1940`-`0x401e1958` | `{0,1,4,3,5,2}` — 6, excludes MANUAL SLICE | `FUN_4005224c` |

Every bound an eighth machine would have to clear, verified against the
section bytes: the dispatch's `moveq #6` at `0x400caf48`, and the four `pea`
immediates at `0x40052000` (table D end), `0x4005200a` (D start), `0x40052296`
(E end) and `0x400522a0` (E start). The dispatch bound is a single byte,
`0x400caf49`, `06` → `07`. **[V]**

That alone buys nothing, because neither array can grow in place. Index 7
resolves to `0x42923644 + 7*0x2c` = `0x42923778`, which is the live
NONE/TRIG/RTRG parameter-page array; and `0x401e1974` is immediately live
vtable/RTTI pointer data. Both neighbours are occupied. The workable shape is
a trampoline (see `docs/PATCHING.md`): relocate table D into the cave with
eight entries and repoint the two immediates, redirect `FUN_400caf48` to cave
code handling `type == 7` while entries 0..6 resolve exactly as now, and build
the 44-byte descriptor there. Note `0x401e1974` also appears at `0x40124252`,
`0x401985dc` and `0x401bae4e` — those refer to the *next* object, which begins
at that address, not to table D's end, and must be left alone. **[O]**

The relocation half of that is done and works. Patching, in guest memory on a
run resumed from `snapshots/boot400M.snap`: an eight-entry table
`{0,1,2,3,6,4,5,7}` written at the cave base `0x402f9c14`, and the two `pea`
operands repointed at it. The firmware then builds its machine-list vector
from the cave — eight reads, eight distinct addresses, all from `0x4012d28e`,
the same vector-copy loop that reads seven entries from `0x401e1958` in an
unpatched control run, which sees zero reads of the original table. The table
reads back intact afterwards, so nothing else claims that memory.
`tools/patchkit/machinepatch.py` runs both arms. **[V]**

The dispatch half is done too. A 48-byte trampoline in the safe cave region
replaces `FUN_400caf48`'s first six bytes with `jmp $40303e5c.l`, adds a
`type == 7` case, and otherwise reproduces the original logic exactly: **[V]**

```
40303e5c  move.l $4(a7), d0          ; the machine type
40303e60  moveq  #$7, d1
40303e62  cmp.l  d0, d1
40303e64  bne.b  $40303e6e           ; not 7 -> original path
40303e66  move.l #$40303f5c, d0      ; the new descriptor, in the cave
40303e6c  rts
40303e6e  moveq  #$6, d1             ; ---- original logic from here
40303e70  cmp.l  d0, d1
40303e72  bcs.b  $40303e84
40303e74  move.b #$2c, d1
40303e78  muls.l d1, d0
40303e7c  addi.l #$42923644, d0
40303e82  rts
40303e84  move.l #$4292374c, d0
40303e8a  rts
```

Calling the patched dispatch directly in the live guest — set up a scratch
stack, `emu_start` at `0x400caf48`, read `D0` — gives the right answer for
every input:

| arg | returns | |
|---|---|---|
| 0..5 | `0x42923644 + arg*0x2c` | unchanged |
| 6 | `0x4292374c` | unchanged |
| **7** | **`0x40303f5c`** | the new descriptor |
| 8 | `0x4292374c` | fallback preserved |

The descriptor carries entry 6's nine fields verbatim, so the new machine
behaves as MANUAL SLICE, but its own name pointers: two static COW reps laid
out in the cave as `[len][cap][-1][chars]`, reading back as `PLACEHOLDER`
(11/11/-1) and `PLHD` (4/4/-1). The `-1` refcount is what makes them safe —
every copy deep-clones rather than mutating cave memory. The run still reaches
post-intro with the patch installed, so nothing about it breaks the boot.
`tools/patchkit/machinepatch.py --milestone b`.

**Installed in the GUI, where the UI actually runs, the list half breaks
boot and the dispatch half does not.** Bisected with
`--patch-machine=list` / `=dispatch`: **[V]**

| patch | result |
|---|---|
| dispatch only | 6 tasks, DTIM3 firing, panel rendering past 340M instructions |
| list only | 2 tasks, DTIM3 never fires, main task in the terminal loop at `0x4012d2fa` |
| neither | 6 tasks, normal |

That clears the trampoline, the descriptor and the two static COW string
reps — and the cave, since the dispatch half writes into the same region.
It also clears the pre-existing `weak_ptr` hang as an explanation: both arms
ran with `--weakptr`, and only the patched one fails. The fault log reports
**0 distinct pages touched**, so it is not a wild pointer either; the
`weak_ptr` trap is an object that was never constructed.

**The terminal loop is not a weak-pointer failure. It is `std::terminate`.**
`0x4012d2fa` is a 2-byte trap with 34 call sites; the GUI's long-standing
"hung on a weak pointer" label is a guess that predates this. A stack scan at
the moment it trips gives one candidate return address, `0x40178484`, which is
the instruction after a `jsr` at `0x4017847e` inside `FUN_40178424`. That
function is part of the **C++ exception unwinder** — `FUN_401772cc`, whose
non-zero return sends it to the trap, parses `.eh_frame`, checking for the
`"eh"` augmentation string and walking CIE/FDE records. So the sequence is: an
exception is thrown, no handler is found, `std::terminate` is called. **[V]**

That explains the otherwise-odd combination of symptoms — an abort with **zero
memory faults**, triggered only by a specific value. A bounds check that
throws is not a wild read.

**The thrower is `std::map<int,int>::at` in the list's sort comparator — not
either `vector::at`.** Found with `tools/guirun.py`, a headless twin of the
GUI's emulator configuration that reproduces the failure exactly (terminal
loop at ~63M, 2 tasks, `DTIM3 0`), by hooking the throw path instead of
guessing at containers. On a `list`-only run: **[V]**

| hook | hits |
|---|---|
| `__cxa_throw` `0x401d5680` | 1, typeinfo `0x40210018` |
| `__throw_out_of_range` `0x401d105c` | 1, message `0x40213a8f` = `"map::at"` |
| `FUN_4019bf70`, `FUN_401ac0fe` (the two `vector::at` candidates) | 0, 0 |

A correction to what this replaces: `0x40225586` and `0x40213a8f` are the
*message strings* `"vector::_M_range_check"` and `"map::at"`, not functions —
`0x40213a8f` is odd, and ColdFire code is word-aligned. Both `vector::at`
candidates `pea 0x40225586` then `jsr 0x401d105c`, which is
`__throw_out_of_range(const char*)`: it allocates the exception and calls
`__cxa_throw` at `0x401d5680`. Hooking those two catches every such throw,
whatever the container. **[C]**

The throwing `at()` is `FUN_40198030`, a `std::map<int,V>::at` (RB-tree walk,
signed-int key at `node+0x10`, value at `node+0x14`) with exactly one caller,
`FUN_400517c4`. A stack scan at the throw gives the chain `FUN_400517c4` <- the
insertion-sort and merge helpers at `0x40051968`..`0x40051e4c` <- `FUN_40051fbc`
at `0x4005207e`, the `jsr` to `__stable_sort_adaptive` right after
`get_temporary_buffer`. **[V]**

So `FUN_40051fbc` does not just copy table D into the list vector. It then
`std::stable_sort`s it, with `FUN_400517c4` as the comparator. That comparator
holds a function-local static `std::map<int,int>` at `0x40984cbc` (guard byte
`0x40984ce8`; `__cxa_guard_acquire`/`release` are `0x401cf7ea`/`0x401cf846`),
filled once by the range insert `FUN_40198948` from seven longword pairs on its
own frame, `[-0x38(a6), a6)`. Read from the instruction bytes, not the
decompiler: **[V]**

| key (machine type) | 0 | 1 | 2 | 3 | 6 | 4 | 5 |
|---|---|---|---|---|---|---|---|
| value (display position) | 0 | 1 | 2 | 3 | 4 | 5 | 6 |

That is the inverse of table D. The comparator returns `map.at(a) < map.at(b)`
in D0's low byte (`sgt.b`, then `neg.l`). With type 7 in the list, the first
comparison involving it calls `map.at(7)`. There is no key 7, so it throws,
nothing catches it, and that is the whole failure.

Ghidra records no references to `0x40984cbc` or `0x40984cc0`, even though both
are absolute `pea`/`lea` operands in `FUN_400517c4`, so `xrefs` on the map
finds nothing; `callers 0x40198948` finds its one writer. **[V]**

This also retires the grouping hypothesis further down. On the failing run,
`FUN_4005d7b8` has **zero** hits before the throw: the sort runs before any
row is grouped. **[V]**

**The fix is patch 5, `rank`: extend the map's initialiser, not its lookup.**
The insert's call site is a 6-byte `jsr $40198948.l` at `0x40051872`.
Repointing its operand at a 22-byte cave shim leaves the comparator, guard,
map, lookups and throw-on-unknown exactly as they were. The shim overwrites the
`[begin, end)` stack arguments with an eight-pair cave table and tail-jumps to
the real insert. The table is `(type, position)` over the list itself, which
reproduces the stock seven pairs and adds `(7, 7)`. Disassembled back from the
emitted bytes: **[V]**

```
40051872  jsr    $403040fc.l          ; was jsr $40198948.l
403040fc  move.l #$4030411c, $8(a7)   ; begin -> cave table
40304104  move.l #$4030415c, $c(a7)   ; end   -> table + 64
4030410c  jmp    $40198948.l          ; the real range insert
```

Its precondition refuses to patch if the guard byte is already set, because
the static would never be rebuilt. All runs are `tools/guirun.py --weakptr`
from `snapshots/boot400M.snap` to 400M instructions: **[V]**

| parts | result |
|---|---|
| none (control) | boots: 6 tasks, `DTIM3` 2038, no throw |
| `list` | terminal loop at ~63M, one `out_of_range` |
| `list+rank` | boots: 6 tasks, `DTIM3` 2034, no throw |
| `list+dispatch+group+name+rank` | boots: 6 tasks, `DTIM3` 2028, no throw |

Its arguments show that type 7 really passes through the comparator rather
than being skipped. On `list+rank` the comparator runs 16 times against the
control's 13; the first ten comparisons are identical in both, and the 11th
and 12th are `(7, 6)` and `(7, 5)`. **[V]**

Narrowing further with `--patch-machine=list:6`, which builds an eight-entry
list whose last entry duplicates MANUAL SLICE instead of introducing a new
machine type: **it boots normally.** So eight entries is fine, and **the
value 7 specifically is what breaks it.** **[V]**

That pointed at `FUN_4005d7b8`, the grouping helper `MachineListView` uses to
place separators. It is not a table lookup but inline branch logic:
`{0,1,2,3,4,6} -> 1`, `{5} -> 2`, and anything `>= 7 -> 0` (via `x &
0xffffff00`, arithmetically zero for 7..255). So machine 7 lands in group
id `0`, which nothing else uses. **[V]**

An earlier version of this section concluded that group `0` is what breaks
boot. **That was wrong.** The value-7 failure is the sort comparator's
`map::at`, described above, which runs before the grouping helper is ever
called, and `list+rank` boots with type 7 still in group `0`. Whether group
`0` renders wrongly (a spurious separator, a missing row) is unobserved,
because no run has drawn the list yet. Patch 3 stays in the set as the
likely rendering fix, not as a boot fix. **[C][O]**

Worth recording as a near-miss: the trampoline's two branch displacements were
wrong on the first attempt — `bne.b` landed on the `rts` rather than the block
after it. Disassembling the emitted bytes back with `tools.coldfire.disasm.disasm` and
checking each branch target lands on an instruction boundary caught it before
it ever ran. The corrected `bcs.b` displacement came out as `65 10`, byte-
identical to the original function's, which is its own confirmation.

What that does *not* show is a row on screen. `FUN_4005e022` (`MachineListView`)
never fires on an idle post-intro run, so the list is built with eight entries
but never drawn without navigation — and Digitakt's post-intro screen renders
blank anyway. The eighth row rendering as a second MANUAL SLICE (index 7 falls
back to entry 6) is an expectation, not an observation. **[O]**

Driving the UI to prove it does not work yet, and the reason is upstream of
this patch. `tools/uidrive.py` installs the Milestone B patch, scripts panel
input, and watches three pixel-free signals: executions of `FUN_4005e022`,
reads of the cave descriptor and its name reps, and calls to the COW copy
`FUN_401d3aba` sourced from the cave. Across an idle window and ten scripted
navigation checkpoints, **all three stay at zero**, in both the patched run
and an unpatched control. The framebuffer stays blank throughout. **[V]**

The panel input itself is fine — every injection produces a shape-valid
`queue_send` record with the right code. The PC, sampled at every checkpoint,
is pinned at `0x40002a18`: `FUN_40002a18`, which sets a PIT2 bit and calls
`FUN_4000148c(0x47d9ade0)` — the **idle task**. So in *these headless runs*
nothing else is runnable and the queued panel events are never consumed. The
control run behaves identically, so this is not something the patch
introduced. **[V]**

Do not generalise that into "the UI never runs": it does. Under `emu/gui.py`
a button click visibly changes the page, so the UI task is scheduled and
consuming panel events there. The difference between the GUI's configuration
and `tools/uidrive.py`'s headless resume is not yet pinned down, and is the
thing to chase before concluding anything about the UI from a headless
run. **[C][O]**

Note `FUN_400607b2` (`MachineSelectionView`) *does* fire, exactly once, ~60M
instructions into a resumed run, in patched and control runs alike. So the
view is constructed and its list vectors are built; only the row-building
`MachineListView` never runs. **[V]**

## Selecting PLACEHOLDER in the GUI **[V][O][C]**

- In `emu.gui --patch-machine --ips-at 80M:18.72M`, FUNC+SRC opens MACHINE
  SEL and DOWN scrolls to PLACEHOLDER. With FUNC released, the
  first YES marks PLACEHOLDER as selected and a second YES closes the
  menu. There is no exception and the main loop keeps up with DTIM3.
  **[V]**
- An earlier version of this section said track 1's SRC page then shows
  SLICE's parameters. That was wrong: LEV, STRT, LEN and LOOP is
  ONESHOT's page, and a run that never opens MACHINE SEL shows the same
  page. With the five parts the track keeps type 0; see "Type 7 did not
  stick" below. **[C]**
- Reaching PLACEHOLDER took 7 DOWN taps from ONESHOT headless, but 4 taps
  in two GUI sessions (in one of them FUNC was latched). Whether FUNC+DOWN
  moves further, or the cursor started on a lower row, is not known. **[O]**
- PLACEHOLDER is machine type 7. The default `MachineSpec` copies the
  nine descriptor fields of type 6, SLICE. With only the five parts those
  fields were never used, because the track's type stayed 0. **[C]**
- Headless, YES on a row that is not the current machine calls
  `0x40035e90` once with (object, track 0, machine type): 7 for
  PLACEHOLDER, 1 for WERP, 4 for GRID. The list stays open afterwards,
  with or without the machine patch. YES on the current machine's row
  closes the list (View::close returns to `0x40060e3c`). **[V]**
- A trig and PLAY on the new machine throw no exception (headless,
  `cxa_throw` hook at `0x401d5680`). Whether it makes sound is not
  checked. The `PLC: ---` popup was not seen after pressing SRC on the SRC
  page. **[V][O]**
- In an earlier GUI session FUNC was still latched, so YES on
  PLACEHOLDER was a FUNC+YES chord. The screen showed "Prj must be
  re-saved!", later a "<project> >> +DRIVE..." screen, and it did not
  change for more than 500M instructions while every task above the
  init task was blocked. A headless replay of that session's recorded
  input, which ended at the YES press, showed neither message, so input
  after that point set it off. **[O]**
- "Prj must be re-saved!" (`0x40225994`) is shown by the project save
  routine `FUN_4004303e` when a cached format code is not 4;
  `FUN_4012184e` reads that code from an in-RAM table. The string
  "INITIALIZING +DRIVE..." is referenced by `FUN_401239be`, a pass that
  reads about 4.1 MiB of the drive in chunks and checks two
  checksum-style gates; the other " +DRIVE..." templates have no static
  reference. **[V]**
- Whether the emulated eMMC, which returns zeros for reads and keeps no
  writes, causes that hang. **[O]**

## The display names are a separate table **[V]**

> **Note 2026-09-28 [V].** The addresses in this section are 1.15C. On 1.16
> the display-name table is a pointer array at `0x4020eb68`; entry 0 points
> to "Oneshot" (`0x402427c6`). Machine type 0 is ONESHOT, a sample machine,
> not "off" (finding 04, "1.16 frame fields"). *P0 2026-09-29:* the table
> has 12-byte entries {long name, short name, extra}, in type order Oneshot,
> Werp, Stretch, Repitch, Grid, MIDI, Slice (read from the raw image; the
> table's only literal reference is at `0x400da55a`). The MIDI = 5 index
> matches the type-5 skip in `FUN_4004e598` (finding 04).

Seeing the machine-select screen render for the first time showed three of the
seven names disagreeing with the descriptors: position 0 reads `ONESHOT` where
the descriptor says `SAMPLE`, position 4 reads `SLICE` where index 6 says
`MANUAL SLICE`, and position 5 reads `GRID` where index 4 says `SLICED SMP`.

The displayed names come from a second, purely static table at `0x401fbc50` —
7 rows of 12 bytes, three big-endian `char*` each, indexed by the **raw**
machine type rather than the UI's display order. These are plain
NUL-terminated C strings, with no COW `std::string` header, so they are a
different mechanism from the descriptor's own name fields: **[V]**

| idx | long | abbrev |
|---|---|---|
| 0 | `Oneshot` | `ONE` |
| 1 | `Werp` | `WRP` |
| 2 | `Stretch` | `STRE` |
| 3 | `Repitch` | `RPI` |
| 4 | `Grid` | `GRD` |
| 5 | `MIDI` | `MIDI` |
| 6 | `Slice` | `SLC` |

Row 5 reuses one pointer for both columns, mirroring MIDI's irregularity in
the descriptor array. Row 6 has a third non-null pointer (`0x4022c7cb`) that
the others lack. It is the header hint `Y:Slice Menu`, read by
`FUN_400dcc9c`; see "Copying SLICE's behaviour to type 7". **[V][C]**

`ONESHOT` was not findable by grep because the stored literal is `Oneshot` —
the UI upper-cases it at draw time.

The accessor is `FUN_400dcc50`, and it has the same shape as the dispatch:

```
400dcc50  moveq  #$6, d1           ; the bound, again one byte
400dcc52  move.l $4(a7), d0
400dcc56  cmp.l  d0, d1
400dcc58  bcs.b  ...               ; out of range -> "ERROR"
          lea.l  $401fbc50.l, a0   ; the table base
```

It is called from `FUN_4005da40`, the invoker half of a `std::function`-style
closure built in `MachineListView`'s constructor and stored per row for lazy
evaluation at draw time — which is exactly why an idle or headless run never
observes it, even though the data is static ROM the whole time.

**This is a fifth bound an eighth machine must clear**, on top of the dispatch
bound and the four `pea` immediates. And the name table cannot be extended in
place: `0x401fbca4`, immediately after row 6, is the base of another table,
referenced by `lea.l $401fbca4.l, a0` at `0x400dcb26`. The 194 zero bytes there
are that table's contents, not slack. So the name table has to be relocated to
the cave as well, with `FUN_400dcc50`'s `lea` immediate repointed. **[V]**

The complete recipe for a visible eighth machine, then, is five patches, all
in `tools/patchkit/machinepatch.py` and selectable part by part with `--patch-machine`:

1. **list** — relocate table D to the cave with an eighth entry, repoint the
   two `pea` immediates. Done and proven.
2. **dispatch** — trampoline `FUN_400caf48` for `type == 7`, descriptor and
   `std::string` reps in the cave. Done and proven.
3. **grouping** — `FUN_4005d7b8`'s exact-6 test becomes a `<= 7` range test,
   so type 7 gets group `1`. Implemented. Not a boot blocker; its rendering
   effect is unobserved.
4. **display name** — relocate the `0x401fbc50` table to the cave with an
   eighth row, repoint the `lea` at `0x400dcc50`, and raise its `moveq #6`
   bound. Implemented, and the table reads back correct; not yet seen drawn.
5. **rank** — give the list's sort comparator a key for type 7, through a cave
   shim on its map's one-time insert at `0x40051872`. Implemented. This was the
   boot blocker, and all five together boot.

`tools/patchkit/machinepatch.py` now takes a `MachineSpec` (display names, descriptor
names, the stock type to copy fields from, list position, optional fields),
and `plan_b` computes every write from a `read(addr, n)` function. With the
default spec it produces the same 18 writes as the verified run, live and
against the static image (`tests/firmware/test_machinepatch_plan.py`). With
`--machine=Lofi:LOF:3:0` the list becomes `{7,0,1,2,3,6,4,5}`, the rank pairs
follow it, boot completes with no exception, and MACHINE SEL shows LOFI as its
first row. The fields copied live from REPITCH's descriptor are
`0, 0xe7, 0, 0xe8, 0xe9, 0xea, 0xeb, 0xec, 0x0a`. **[V]**

## Type 7 did not stick: a permission check was the sixth bound **[V][C]**

The five parts above make the machine visible and selectable, but not
used. YES on PLACEHOLDER calls the commit `0x40035e90` with type 7, and the
track keeps type 0 (ONESHOT). A trig shows the header `Oneshot`, and the
descriptor dispatch `0x400caf48`, called from `0x40017674`
(`FUN_4001762c`), gets type 0 for all 1467 calls in the run. Selecting
SLICE changes that argument to 6 on the first call after the second YES.
SLICE runs with and without the patch give identical screens and commit
calls. **[V]**

- The commit calls the setter `FUN_40050cd6(slot, type, track, flag)`
  with `slot = obj + track*0x3c0 + 0x6c`. The setter asks
  `FUN_400dcab8(type, FUN_400dcb5e(track))` whether the type is allowed.
  On 0 it returns at `0x40050cfe`, before it stores the type byte at
  `+0xa2` of the object returned by the slot's vtable `+0x28`. **[V]**
- `FUN_400dcab8` rejects type > 6 (`moveq #6,D2` at `0x400dcaba`, then
  `bcs`), then tests bit `mask` of the first word of a 7-long table at
  `0x401fbda6`. All seven entries are `0xffff`, so only the bounds reject
  anything today. The slot after type 6 overlaps the track table at
  `0x401fbdc0`. **[V]**
- Headless, selecting SLICE writes `0x00` -> `0x06` at `0x4263b1a2`
  (track 0) at instruction 190283779. Selecting PLACEHOLDER takes the
  early return at 223538984 and writes nothing. **[V]**
- `FUN_400dcab8` has six other callers: `FUN_4002cd90` (assign a
  machine), `FUN_40035a34` (per-type track availability), `FUN_400361b4`
  (sound load), `FUN_4004fc52`, `FUN_400513f6` (paste sound) and
  `FUN_400d8f26` (sound locks). **[V]**
- The `permit` part copies the table to cave B `+0x1a0` with an eighth
  entry from `clone_of`, repoints the `lea` at `0x400dcad0` and raises the
  bound to 7. With it the dispatch gets type 0 for the 396 calls before the
  second YES and type 7 for all 1026 after, the setter never takes the
  early return, and the SRC page shows SLICE's LEV, SLICE, LEN and `---`.
  **[V]**
- `FUN_4001762c` pushes one argument to `0x400caf48`. The second and third
  stack words seen by a hook there are left over from an outer frame.
  **[V]**

## Copying SLICE's behaviour to type 7 **[V][O]**

With type 7 stored, the track still differed from SLICE where the firmware
tests the type number. Three more parts in `tools/patchkit/machinepatch.py`, driven
by `MachineSpec.clone_of`, cover the differences found so far. Bare
`--patch-machine` applies all nine parts.

- `hint`: the name table at `0x401fbc50` has a third column, the header
  hint shown after a trig. The accessors `FUN_400dcc76` (short name, `+4`)
  and `FUN_400dcc9c` (hint, `+8`) have the same `moveq #6` bound as
  `FUN_400dcc50`, and use `addi.l #table+column,D0` at `+0x12`. Out of
  range the hint is `"READ ERROR"+5` (`ERROR`, drawn as `ERR`) and the
  short name is `ERR/ERR`. The part raises both bounds, points both at the
  relocated table and gives row 8 `clone_of`'s hint. **[V]**
- `pertype`: a 7-byte table at `0x401d9f30` (`cd d6 df e8 f1 00 fb`; the
  next byte starts an unrelated string) is read behind a `moveq #6` bound
  by `FUN_400166b8`, `FUN_40017828`, `FUN_40017b56` (twice),
  `FUN_40017080` and `FUN_40016624`; type 7 gets 0. The part moves it to
  cave B `+0x1c0` with `clone_of`'s byte as the eighth and raises the six
  bounds. What the bytes mean is not known. **[V][O]**
- `clone`: each firmware test of `type == 6` jumps to a shim in cave B at
  `+0x300` that repeats the compare and also accepts 7: `FUN_4005f0c0`
  (the Slice menu entry), `FUN_4005cb7c` (`(type & ~2) == 4`, types 4 and
  6), `FUN_4005be94` (step count), `FUN_4003065a` and `FUN_40048660`
  (parameter `0xfc`) and `FUN_4005edd6` (a per-track loop). Only SLICE's
  tests are listed; for another `clone_of` the part writes nothing. **[V]**
- Headless with all nine parts, a trig shows `Placeholder  Y:Slice Me`
  (cut at the screen edge), and YES on the SRC page opens the Slice menu
  (EDIT SLICE POINTS, CREATE SLICE GRID, CREATE LINEAR LOCKS, CREATE
  RANDOM LOCKS) through the `FUN_4005f0c0` shim. No exception. **[V]**
- Not exercised: the `FUN_4005cb7c` shim was not reached, and the
  step-count, parameter-`0xfc` and per-track-loop sites were not hit.
  `FUN_40017080` has its own type-6 case (object `+0x13c`) that is not
  patched. After a trig SLICE draws a horizontal line that PLACEHOLDER
  does not. **[O]**
- Tests for other types (`== 4` in `FUN_4003065a`, `FUN_40048660`,
  `FUN_4005be94`, `FUN_4005e788` and `FUN_4005f0c0`) are known but not in
  the part. The search covered the 32 callers of the type reader
  `FUN_4004fc02` and the xrefs of the two tables, not every read of
  `+0xa2`. **[O]**
- What the SHARC is told about a type-7 track, and whether saving a
  project with type 7 works, are not checked. **[O]**

## Digitone II 1.11 has the same machine machinery, with five machines **[D]**

Every anchor of the Digitakt II machine machinery has a Digitone II 1.11
counterpart of the same shape with different data. The addresses for all three
mapped images are in `tools/patchkit/machineprofile.py`.

- Five machine types, not seven: `0 FM Tone`/`FMT`, `1 WaveTone`/`WVT`,
  `2 FM Drum`/`FMD`, `3 Swarmer`/`SWM`, `4 MIDI`/`MIDI`. The display-name
  table is at `0x401f77f0`, the same 12-byte rows of three `char *` as
  Digitakt's, with the hint column null on every row. **[D]**
- The descriptor dispatch is `FUN_400c248e`: bound 4, stride `0x2c` -- **the
  same 44-byte descriptor as Digitakt** -- array base `0x42432b24`. Its
  out-of-range fallback `0x42432bd4` is MIDI's own descriptor, which is also
  what index 4 computes to, so MIDI is not special-cased. Rows 0-3 are
  confirmed by the registration function `FUN_400c2aea`, which `pea`s each
  machine-name string next to the matching row address. **[D]**
- The type byte is at `+0xde` of the per-track object, not `+0xa2`. The setter
  `FUN_4004cc08` reaches the object the same way, through the slot's vtable
  `+0x28`. **[D]**
- The permission check `FUN_400dc19a` has the same shape and its mask table
  `0x401f7932` is five entries of `0xffff`. The grouping helper `FUN_40059274`
  has the same shape too: synthesis types to group 1, MIDI to group 2,
  anything else to 0. **[D]**
- Two of the three name accessors, at `0x400dc358` and `0x400dc37e`, are not
  Ghidra functions at all -- they sit in the gap after `FUN_400dc332`. The
  same is true of the two Digitakt ones. Disassemble the gaps. **[D]**
- Three things Digitakt has were not found: the six-long list filter table,
  the sort comparator with its function-local `std::map` (there is no
  `stable_sort` anywhere in the image, and with five static entries in a fixed
  source order there may be no runtime sort to break), and the per-type byte
  table. A patch template must treat all three as optional. **[O]**
- Digitone II has a per-machine parameter-page layer Digitakt has no
  counterpart for: `MachineParameterPageView`, `SrcMachineParamPageCopy`,
  `FilterMachineParamPageCopy`. Its machines are synthesis engines with their
  own parameter pages, so a sixth machine there is a materially bigger job
  than an eighth on Digitakt -- a descriptor and a name row will not be
  enough. **[O]**

## The 1.16 machine and parameter descriptor tables, fully extracted **[V]**

Two static tables define what a "machine" is on the ColdFire side, and both are
now dumped deterministically by tools in the repo (2026-09-20). A second agent
re-decoded the raw entry bytes, the reader's disassembly and every descriptor
write site by hand and matched both tools exactly, so these are [V].

**Machine descriptor table** -- base `0x4293b960`, stride `0x2c` (44 B), 7
entries, dispatch `FUN_400c8840` (`moveq #6` bound, out-of-range falls back to
`0x4293ba68` = entry 6). Entry layout: `+0` and `+4` are two name pointers,
`+8 + n*4` are nine ID fields (`n` = 0..8). The accessor is inlined in
`SourcePageView::vfunc_47` (`decomp/40017cd4_SourcePageView__vfunc_47.c:10-14`):

```c
uVar2 = FUN_4005063e(uVar2);      // track's machine type
iVar3 = FUN_400c8840(uVar2);      // dispatch(type) -> descriptor
uVar2 = *(undefined4 *)(iVar3 + 8 + param_2 * 4);
```

The table is populated at boot by the initializer at `0x401bdee2`, which writes
the IDs as immediates. `tools/patchkit/machinedescr.py` parses that initializer (with a
bitmask reaching-definition scan for the register-borne stores) and resolves the
name pointers against the raw image. Entry 6 reproduces `machinepatch.py`'s
1.15C `ENTRY6_FIELDS` byte for byte, and each resolved name address is also an
independent rodata grep hit.

| type | name | short | f0 | f1 | f2 | f3 | f4 | f5 | f6 | f7 | f8 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | SAMPLE | SAMP | 0xca | 0xcb | 0 | 0xcd | 0xce | 0xcf | 0xd0 | 0xd1 | 0x0a |
| 1 | WERP | WERP | 0xd4 | 0xd5 | 0 | 0xd6 | 0xd7 | 0xd8 | 0xd9 | 0xda | 0x0a |
| 2 | STRETCH | STRETCH | 0xdd | 0xde | 0 | 0xdf | 0xe0 | 0xe1 | 0xe2 | 0xe3 | 0x0a |
| 3 | REPITCH | REPITCH | 0 | 0xe7 | 0 | 0xe8 | 0xe9 | 0xea | 0xeb | 0xec | 0x0a |
| 4 | SLICED SMP | SLIC | 0xef | 0xf0 | 0 | 0xf1 | 0xf2 | 0xf3 | 0xf4 | 0xf5 | 0x0a |
| 5 | MIDI | MIDI | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | MANUAL SLICE | MLIC | 0xf8 | 0xf9 | 0 | 0xfb | 0xfc | 0xfd | 0 | 0xfe | 0x0a |

The contiguous table at `0x4293b858` (base + -6*0x2c, dispatch `FUN_400c8862`,
bound `moveq #5`, same fallback) is **not** a second machine list -- its names
decode to STATE VARIABLE / LOWPASS 4 / EQUALIZER / COMB- / LEGACY LP/HP / COMB+,
i.e. the filter types. This corrects the working assumption that it was the
"filter-page machine list" in the 1.15C sense of a second machine table.

**Parameter descriptor table** -- base `0x4020f18c`, stride `0x3c` (60 B),
`0x113` (275) entries, indexed by param_code. Reader `FUN_400d9ed8` returns the
field at `+4`. `tools/coldfire/paramtable.py` dumps and decodes it. Entry layout:

| off | field | evidence |
|---|---|---|
| +0x00 | `owner_type` (machine/filter type, -1 = global) | matches the descriptor tables 1:1 |
| +0x04 | `mirror_index` (-1 = not mirrored) | `FUN_400d9ed8` |
| +0x08 | `min` | 12-byte memcpy of `+8..+0x13` in `FUN_400da204` |
| +0x0c | `max` | same memcpy |
| +0x10 | `default` | same memcpy; `ParameterPageView::vfunc_2` passes it to the value setter on "Clear" |
| +0x14 | `flag` (bool) | `FUN_400da48e`; passed as a boolean to the value formatter **[D]** |
| +0x18 | i16 | `FUN_400da4b8` (signed 16-bit return) |
| +0x1a | i16 | `FUN_400da4dc` |
| +0x1c | `kind` | `FUN_400da500`, fed to `FUN_40121994` as a unit/transform selector |
| +0x20 | unknown | accessor `FUN_400da330`, no caller located **[O]** |
| +0x24 | unknown, used as a bitmask elsewhere | accessor `FUN_400da524` **[O]** |
| +0x28 | `long_name` (char*) | resolves to display strings |
| +0x2c | `owner_name` (char*) | resolves to "Sample", "Stretch", "MSlice", ... |
| +0x30 | `short_name` (char*) | resolves to the 4-char page abbreviation |
| +0x34 | formatter (code ptr) | `0x400e****`, shared between equivalent params across machines **[D]** |
| +0x38 | constant `0x4023f364` in every entry sampled | **[O]**, likely a sentinel |

All 275 entries resolve a long display name.

## A machine is eight fixed engine slots, not eight free parameters **[V]**

Joining the two tables gives the complete SRC page for every machine, and the
structure it reveals is the important part: **`mirror_index` is positional, not
per-parameter.** Descriptor field `n` always lands on the same mirror index, on
every machine and on the filter table too:

| descriptor field | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|---|---|---|---|---|---|---|---|---|---|
| mirror index (machines) | 25 | 26 | **27** | 28 | 31 | 32 | 33 | 34 | -1 |
| mirror index (filters) | 40 | 41 | 42 | 43 | 36 | 37 | 35 | 38 | -1 |

So a machine does not choose *where* its parameters go; it chooses *what each
fixed slot means* -- the name, range, default, unit and formatter shown for that
slot, plus whatever ColdFire-side math converts the user's value before it is
mirrored. That is why SAMPLE's `STRT` (0..0x7800) and SLICED SMP's `SLICE`
(0..0x4000) share mirror index 31: the engine input is the same, the
interpretation is not. Field 8 is `0x0a` (Track Level, `mirror_index` -1) on
every machine and every filter -- a shared trailer, not an SRC-page slot.

The joined table (`tools/patchkit/machinedescr.py` + `tools/coldfire/paramtable.py`):

| slot | mir | SAMPLE | WERP | STRETCH | REPITCH | SLICED SMP | MANUAL SLICE |
|---|---|---|---|---|---|---|---|
| 0 | 25 | TUNE | TUNE | TUNE | -- | TUNE | TUNE |
| 1 | 26 | PLAY | PLAY | PLAY | PLAY | PLAY | PLAY |
| 2 | 27 | -- | -- | -- | -- | -- | -- |
| 3 | 28 | SAMP | SAMP | SAMP | SAMP | SAMP | SAMP |
| 4 | 31 | STRT | SEG | STRT | STRT | SLICE | SLICE |
| 5 | 32 | LEN | MODE | LEN | LEN | LEN | LEN |
| 6 | 33 | LOOP | BARS | BARS | BARS | GRID | -- |
| 7 | 34 | LEV | LEV | LEV | LEV | LEV | LEV |

MIDI (type 5) has all nine fields zero -- it is not a sample engine at all.

## Two described-but-unexposed parameters: CFADE and REPITCH's TUNE **[V]**

Slot 2 / mirror index 27 is `clr.l` on all seven machines -- verified as literal
`clr.l` instructions at `0x4293b970`, `0x4293b99c`, `0x4293b9c8`, `0x4293b9f4`,
`0x4293ba20`, `0x4293ba4c`, `0x4293ba78`, not a tool inference. But the
parameter table contains two complete descriptors for that slot:

- param_code `0xcc` @ `0x4021215c`: long_name "Crossfade", short_name "CFADE",
  owner "Sample" (type 0), mirror 27, min 0, max `0x7f00`, default 0,
  kind `0x81`, formatter `0x400e1424`.
- param_code `0xfa` @ `0x40212c24`: identical, owner "MSlice" (type 6).

Both were hand-decoded from raw hex and their string pointers resolve to genuine
NUL-terminated rodata in the dedicated param-name block
(`...le Bank\0Crossfade\0CFADE\0MSlice\0Sample-R...`). Nothing in MAIN OS
references either param_code: `xrefs.sqlite` `data_refs` has zero hits inside
either entry's `0x3c` span, and `tools/coldfire/refscan.py` over the whole image (96.78%
byte coverage) found one apparent hit which was run down and shown to be a
linear-sweep desync inside an RTTI blob (the literal big-endian target word does
not occur there). The only thing that touches them is `FUN_4004fefc`, which
sweeps all `0x113` entries unconditionally. "Crossfade"/"CFADE" is **also
present in 1.15C** (file offsets `0x22c691` / `0x22c69b`), so it is long-dormant
rather than a 1.16 feature in progress. DN2 1.11 has neither string.

Separately, REPITCH's TUNE (param_code `0xe6`, mirror 25) is described but
deliberately disabled: its descriptor field 0 is a literal `clr.l` at
`0x4293b9ec`, and its table entry is the outlier on three independent fields --
`kind` = `0xffffffff` where every other TUNE has `0x80`, `i16@0x18` = -1 where
every other TUNE has 16, and `+0x24` = 0 where every other TUNE has `0x1e00`.
Musically consistent: REPITCH derives pitch from length.

An aside worth recording so it is not re-investigated: there is no standalone
"TUNE" string in the image. Both occurrences are suffix-merged tails inside an
embedded English word list ("MISFORTUNE", "OPPORTUNE"). That is ordinary linker
string-suffix merging, not a bad pointer.


## Exposing CFADE: scoped, two six-byte edits **[D][O]**

What it would take to turn on the dormant slot-2 parameter, scoped but **not
applied**. Nothing here has been patched or tested.

Descriptor field 2 is written by a `clr.l` in the boot initializer, so the patch
target is that instruction, not a table byte:

```
SAMPLE (type 0)       guest 0x401c4844  file 0x1c4444  42 b9 42 93 b9 70  clr.l $4293b970.l
MANUAL SLICE (type 6) guest 0x401c4b08  file 0x1c4708  42 b9 42 93 ba 78  clr.l $4293ba78.l
```

Both are 6 bytes. The replacement `move.l #$cc,$4293b970.l` is **10 bytes**, so it
does not fit, and the following instruction cannot be borrowed: it loads
`0xcb`/`0xf8` for a different field's store a few instructions later, and
shrinking it to a `moveq` would sign-extend to `0xffffffcb`, which
`FUN_400d9ed8` would then clamp to entry 0 and silently corrupt that other slot.

The fit is a 6-byte `jmp (cave).l` (`4ef9` + address) at each site, with a 16-byte
stub in the confirmed-zero cave tail (`tools/patchkit/machineprofile.py` `cave_b =
0x4031be5c`, free to end of file, and the existing eighth-machine patch occupies
only the first ~`0x380`):

```
stub: 23 fc 00 00 00 cc 42 93 b9 70   move.l #$cc,$4293b970.l
      4e f9 40 1c 48 4a               jmp $401c484a.l
```

Two length-preserving 6-byte edits plus 32 bytes of new code in previously-zero
space. Much smaller than the eighth-machine work: no list, dispatch, rank, permit
or clone parts, because SAMPLE and MANUAL SLICE already exist and are already
permitted -- only a dormant field is being turned on.

Gates a newly non-zero field 2 must pass:

| gate | status |
|---|---|
| descriptor field 2 | needs patching |
| param-table entry for `0xcc`/`0xfa` | already complete **[V]** |
| row/parameter registration `FUN_40049b16` | satisfied -- walks all nine fields, skips only `0` and `0xa` (`moveq #0xa,D1; move.l (A3)+,D0; cmp.l D0,D1; beq; tst.l D0; beq`) **[D]** |
| encoder/touch hit-test `FUN_4006216e` | satisfied -- same zero-skip shape, independent consumer **[D]** |
| machine-type permission check | not applicable; types 0 and 6 already permitted **[V]** |
| DSP transport | satisfied -- unconditional copy, index 27 to frame `+0xde` **[V]** |
| a SHARC-side consumer that does something audible | **unknown [O]** |

Persistence looks generic rather than enumerated: `FUN_4002d7a4` is bound-checked
`idx < 0x47` and index 27 sits inside both the `Sound + 0x14 + idx*2` array and
the mirror, and the live kit table is flat POD that undo/copy `memcpy` wholesale.
The flash-to-RAM project-load routine and the on-flash serialisation format were
never traced, so whether a previously-always-zero index survives a save and
reload is **[O]**.

The one risk that matters is the last gate: whether the SHARC implements a
crossfade with that value or ignores it the way REPITCH's deliberately disabled
TUNE is ignored. Recommendation is to do SAMPLE first -- equally cheap, and a
quieter neighbourhood than MANUAL SLICE, which sits next to the slice-boundary
sub-block and the type-4/6 direct STRT feed.

## XSLICE: a flashed eighth machine with its own SRC-page control, on Digitakt II 1.16 **[V][O]**

`tools/patchkit/machinebuild.py --profile dt2-1.16 --machine XSLICE:XSL:6:7 --fields
0xf8,0xf9,0xfa,0xfb,0xfc,0xfd,0,0xfe,0x0a` builds a flashable 1.16 image
with an eighth machine: a MANUAL SLICE clone (type 7) whose descriptor field
2 is `0xfa`, MANUAL SLICE's orphaned CFADE row (owner 6, mirror 27, 0 to
0x7f00). All nine machinepatch parts go into `flash_cave` (952 of 2108
bytes); section 3 sha256 `cdbd7726...ecb86a`.

Booted from reset in the emulator (400M rung) and driven from the panel,
with stock SLICE on the stock image as the control:

- FUNC+SRC lists XSLICE below MIDI (group 1, after the MIDI separators).
- In `MachineSelectionView::vfunc_2` (`0x40061678`), a YES that changes the
  selection commits and does not close; a second YES closes. The commit
  reaches the setter `0x40051712` with type 7 for track 0, permit
  `0x400da3b0` returns 1, and `move.b D2,(0xa2,A0)` at `0x4005179e` writes 7
  to the track object (`0x4265338e`), where it stays for 50M instructions.
  Stock SLICE takes the same path with 6.
- On the SRC page, XSLICE shows a CFADE knob at slot 2 where SLICE shows an
  empty box, and encoder C (wire channel 2) moves it; the same input does
  nothing on SLICE.

Open: the TX frame words (type `0x94+2t`, CFADE `+0xde`) read zero on both
images, even with the frame-build gate `0x409664f4` open, because nothing in
the harness raises vector 191 on its own (`tools/sharcframe.py` raises it by
hand). Whether CFADE is audible is also open. `SoundParameterSet::vfunc_21`
(`0x4003ae02`) compares a parameter's owner (6) with the track type (7); its
callers are the modulation-destination views, so XSLICE's parameters may be
missing from modulation pickers. **[O]**

## The parameter descriptor table by signature, and where FX values live **[V]**

Checked 2026-10-08 against the image bytes by a second agent, and by the
Ghidra dumps `dt2-1.16-emac` and `dn2-1.11-emac`.

**Finding the table.** The table's reader is a 36-byte function that occurs
exactly once in each image:

```
20 2f 00 04  0c 80 00 00 CC CC  55 c1  41 f9 BB BB BB BB
73 01 c0 81  22 00 e5 89 ed 88 90 81  20 30 08 04  4e 75
```

`CC CC` is the entry count and `BB BB BB BB` the table base. Stride 0x3c.
Entry 0 is "Error"/"ERR" with owner and mirror index 0xffffffff, and entry 6
is "Machine Type".

| Image | Reader | Entries | Base |
|---|---|---|---|
| DT2 1.16 | `0x400d9ed8` | 0x113 (275) | `0x4020f18c` |
| DT2 1.15C | `0x400dc5e0` | 0x113 | `0x401fc274` |
| DN2 1.11 | `0x400dbcc4` | 0x141 (321) | `0x401f7f94` |
| DN2 1.10E | `0x400de12c` | 0x141 | `0x401e29a0` |

**FX parameters.** Codes 0x43..0x9f have the same long names on DT2 1.16 and
DN2 1.11. Owner type ids differ between the images (FX is 0x11 on DT2 and 0xf
on DN2), so tools should match parameters by name, never by owner id. All
values are s16 in 0..0x7f00, and the UI value is the high byte.

| Parameter | DT2 1.16 code / mirror | DN2 1.11 code / mirror |
|---|---|---|
| Delay Send, Reverb Send, Chorus Send | 0x44, 0x45, 0x43 / 55, 56, 57 | same codes / 86, 87, 88 |
| Delay Time (default 0x1700) | 0x71 / 32 | 0x71 / 32 |
| Feedback Gain (default 0x2000) | 0x74 / 35 | 0x74 / 35 |
| Reverb Decay Time (default 0x1f00) | 0x7c / 42 | 0x7c / 42 |
| Bit Reduction, Sample-Rate Redux, SRR Routing (max 0x100), Overdrive | 0xff..0x102 / 64..67 | 0x12d..0x130 / 95..98 |

**Value storage.** `mirror_index` indexes an s16 array: 71 entries on DT2
(`SoundParameterSet`/`FxParameterSet::vfunc_10` bound it with `< 0x47`), 101
on DN2 (`< 0x65`). The arrays live in the live kit blob `kit_t`. Each track
has its own Sound array, which holds the track-level owners: machine,
filter, amp, and the track FX (sends, BR, SRR, overdrive). The kit has one Fx
array, which holds chorus, delay, reverb, master, input and the LFOs. The
pointer chains below were read from the 2026-10-07 ready snapshots. Who
writes the globals has not been traced. **[V at ready]**

| | DT2 1.16 | DN2 1.11 |
|---|---|---|
| `kit_t` | `*0x80004704` | `*0x800052a0` |
| Fx array | `*0x80004708` (= kit_t + 0x4534) | `*0x800052a4` (= kit_t + 0x48e4) |
| Sound array, track j | `*(0x8000470c + 4j) + 0x14` (= kit_t + 0x34 + j*0x450 + 0x14) | kit_t + 0x34 + j*0x48b + 0x14 (odd stride; the 0x800052a8 slots all hold track 0) |

The DT2 per-track UI mirror at `0x80003362 + t*0x8e` is a copy:
`FUN_4002d438` memcpys 0x8e bytes from `Sound + 0x14` into it. Read values
through the chain above, not the mirror. **[C]** The odd DT2 card kit was
the firmware's factory demo kit. `tools/firmware/plusdrive.py` copies the built-in v3
project (ELZ stream at 0x4025F1D8), and kit 0 of that project is "KIT 1",
with edited FX: master overdrive 0x7f00, compressor on, reverb decay 0, and
a detuned track. Both writers now copy kit 32 ("PRESET 1") over kit 0 and
keep the sample references. With that fix the card kit matches a card-less
boot in the FX block, and the 1 kHz test sample plays at 999.94 Hz with
harmonics below -89 dB. Fx idx 0..24 still differ from a card-less boot:
the v3 kit does not carry them. **[V]**

**On DT2 1.17 and DN2 1.12 the chain is the same.** The setters of `kit_t`, the Fx array and the
Sound pointers (DT2 `FUN_4002d3e0`, `FUN_4002da7a`, `FUN_4002d9c4`; DN2 `FUN_40025baa`, `FUN_40025444`,
`FUN_40025af4`) sit at the same addresses with the same bytes in both versions of each device, so the globals,
offsets and sizes above are unchanged, and so are the BR / Delay Send mirrors (DT2 64 / 55, DN2 95 / 86). The
descriptor table moved by +0x410 (DT2 0x4020f18c -> 0x4020f59c, DN2 0x401f7f94 -> 0x401f83a4; counts 0x113 /
0x141); `paramtable.find_table` finds it by signature on all four images. The section 7 DSP blobs are
byte-identical between 1.16 and 1.17 and between 1.11 and 1.12. With the ready snapshots rebuilt on 1.17 /
1.12 (2026-10-20), every PCM pin and every oracle test (61 DN2, 135 DT2) gives the same result as on 1.16 /
1.11. **[D]**

`tools/coldfire/cf_names.py` labels `0x8000470C` as `track_sync_cache`. At ready it
holds the 16 Sound pointers. **[O]**

The ready snapshots are Rust `DT2SNP01` files. `tools/emulator/snapread.py` reads
their memory, but not their registers or tasks.

## Persistence of machine types and parameter values (static) (loader immediate, record offsets, tables, kit constants [V]; emulator round-trip steps [D])

Question: does a new machine type (id 7 on DT2) and a new parameter value survive save and load, and what does a
stock firmware do with a file from a patched one? Read from the Ghidra 1.16 dump (`dt2-1.16-emac`) and the 1.11
dump (`dn2-1.11-emac`); image bytes read for the immediates below. Nothing was run. All marks are **[D]** unless
stated. Addresses are 1.16 unless stated. A second agent verified **[V]**: the loader immediate and its four
addresses (DT2 1.16 `0x400db2eb` = 08, 1.17 `0x400db44f` = 08, DN2 1.11 `0x400dd287` = 06, DN2 1.12 `0x400dd3eb` =
06), the record offsets (`+0xd2` -> `+0xa2` on DT2, `+0xf4` -> `+0xde` on DN2), both permutation tables with their
bounds `0x8a` and `0x9a`, the kit constants, and the type `0xff` / 7..254 behaviour.

**[C] Address base of the sections above.** The dispatch and permission addresses in the earlier sections of this file
(`0x400caf48`, `0x400dcab8`, `0x401fbda6` and so on) are DT2 **1.15C** addresses: the 8-byte pattern
`7206 202f 0004 b280` is at `0x400caf48` in the 1.15C image and at `0x400c8840` in 1.16 (`0x400da372`-group is
shifted by -0x2708 there). The 1.16 names are:

| role | 1.15C | 1.16 | 1.17 | DN2 1.11 |
|---|---|---|---|---|
| descriptor dispatch (bound `moveq #N,D1` at entry) | `0x400caf48` | `FUN_400c8840` (N=6) | `0x400c89a4` | `FUN_400c248e` (N=4, stride 0x2c) |
| machine permission (`moveq #N,D2` at entry+2) | `0x400dcab8` | `FUN_400da3b0` (N=6, table `0x4020ecbe`) | `0x400da514` | `FUN_400dc19a` (N=4) |

DN2 1.11 has five machine types (0..4) at both gates. Matches come from `tools/coldfire/cfport.py` (new; wraps
`tools/coldfire/cfmatch.py`): 1.17 matches are all `strict`; the DN2 1.11 ones are `loose` or `callee` and were confirmed by
reading the decompile and disassembly of each.

### Storage records

The files and the in-memory record share one versioned family, the shared-library classes
`Digisharc::*Storage_vN_t` (RTTI `ValueWithMirror<runtime_t, storage_t>`: the runtime object is the master, the
storage struct is a mirror that `Sound::updateMirror` and the other `updateMirror` functions fill). The
`Digitakt::*Storage_vN_t` names (`projectStorage_v1..v16`, `kitStorage_v1..v10`, `soundStorage_v1..v3`,
`patternStorage_v1..v10`, `fxSetupStorage_v1..v3`, `projectSettingsStorage_v1..v8`, `songStorage_v1`) are a
different, older family: sound 0xa0 bytes, kit with 8 tracks, magic `0xbeefbace`/`0xbacef00c`, and it is only the
**import** path for old projects (`FUN_4001504e` project v16 -> current, `FUN_40014d20` kit, `FUN_4001498a` sound,
`FUN_40015a3e` sound container; error strings "Failed to upgrade source project! Version = %d, expected %d").
The native DT2 and DN2 format is `Digisharc::projectStorage_v5_t` (version **5**) with these parts:

| part | DT2 1.16 | DN2 1.11 |
|---|---|---|
| project | version 5, head magic `0xbeefbace` at +0, version at +4, tail magic `0xbacef00c` at word `0x376580` (+0xDD9600), size 0xDD9604 | same version, tail at word `0x312c00`, size 0xC4B004 |
| pattern | 128 x 0x15c00 at +0x200; 16 tracks x 0x4a0 | 16 tracks x 0x4a3 |
| kit | 129 x 0x5800 (storage), runtime stride 0x5574; version 4 at +4, tail `0xbacef00c` at +0x573a | same layout, different fx block |
| sound pool | 130 x 0x455 | |
| **sound** | **0x455 bytes**, version 3 | **0x167 bytes**, version 3 |

Kit sounds are 16 x 0x455 (DT2) inside the kit at +0x3c, stride 0x455; the sound pool uses the same record.

**Sound record (DT2 `soundStorage_v3`, 0x455 bytes) [D].** +0 `0xbeefbace`, +4 version (3), +8 u32 id, +0xc name[16],
**+0x1c .. +0xd2 parameter slots (s16, big endian; 91 slots, 77 used)**, **+0xd2 machine type (u8)**, +0xd3 a second u8
(stored 0xff or 0..5), +0xd4.. small enums, +0x139 64 x 12 bytes sample-slot data, tail `0xbacef00c` at +0x451.
DN2: params at +0x1c .. +0xf4 (108 slots, 107 used), **machine type at +0xf4**, tail at +0x163.

**Where the machine type lives.** In the sound record only (runtime `Sound + 0xa2` on DT2, `+0xde` on DN2; kit
track j is `kit_t + 0x34 + j*0x450` on DT2). Kits hold 16 such records and the sound pool 130, so the type is
persisted per kit track and per pool sound. Pattern tracks (`FUN_400dbf66`, 0x4a0 per track) hold trig and
parameter-lock data and no machine field. `trackStorage_v2` (`Track::updateMirror`) was not traced **[O]**.

**Parameter values.** Raw s16 in the slot array. The runtime array is `Sound + 0x14`, 71 entries (0x8e bytes) on
DT2, 101 on DN2. Two permutation tables map runtime index and storage slot:

- save, `FUN_400db930` (runtime -> storage, called from the background worker `FUN_4004fd74` and the `FUN_400dbc66`
  kit saver): loops 0x8a bytes = 69 entries over the table at `0x40213640` (values 0..76), `storage[(tbl[i]+0xe)*2]`.
  Runtime indices 69 and 70 are not in the table: **never saved** **[D]**. The save zero-fills the 0xb4 byte block
  first, so unused slots are 0.
- load, `FUN_400db24e` (storage -> runtime): loops 0x9a bytes = 77 slots over the table at `0x40213754`
  (values 0..68), `runtime[tbl[j]] = slot[j]`. Slots whose entry is 0 are padding (0, 4, 8 ... 32); runtime index 0
  is overwritten by them and is a dummy.
- 3 of the values (runtime indices 4, 12, 20, the same set `Sound::updateMirror` re-maps) are parameter ids, not
  levels: both directions translate them through the table (`tbl[(signed char)(v>>8)] << 8`), unchecked, so a
  value outside 0..76 reads past the table in rodata (bounded, no fault) **[D]**.
- pattern parameter locks (`FUN_400dc4a6` load, `FUN_400dc6a0`/`FUN_400dc7ea` save): a lock entry is 0x102 bytes
  (id, track, 128 x s16). On load an entry with id >= 0x4d (77) or track >= 0x10 is skipped; the id goes through
  `FUN_400dad24`, which returns 0 for an id out of range (tables `0x40213754` for tracks, `0x40213588` for the FX
  track, bound 0x2e). Values below 0 mean "no lock". Locks are kept in `lock[track*0x47 + runtime_idx]` (0x47 =
  71 per track), so a runtime index must be below 71 to be lockable.

### What load does with an unknown machine type or value

The one load function for a sound record is `FUN_400db24e` (1114 bytes). Every path ends in it: project load
(`FUN_400df7f6` -> sound pool at +0xda5a04), kit load (`FUN_400db734`, 16 sounds), kit/sound container and bank load
(`FUN_400db6cc` for 130 sounds, `FUN_400dfd68` after the sound v1/v2 -> v3 upgrade `FUN_400dfc8e`), and the
`MmcFs::vfunc_12` file path. **[D]**

| input | result | evidence |
|---|---|---|
| sound version field != 3 | `FUN_400db24e` returns 0 (no magic check at sound level); caller resets that one slot to a default sound (`FUN_400e6494` for kit tracks, `FUN_400e6378` for pool, `FUN_400e6494(...,i)` in `FUN_400db6cc`) | `cmpi #3` at entry |
| **machine type byte** | `runtime[0xa2] = b & mask`, mask = 0xff when `(u8)(b+1) < 8` else 0: **0xff and 0..6 are kept, 7..254 become 0** (SAMPLE). Not rejected, not clamped to the top, not used as an index | `move.b (0xd2,A3)`, `addq #1`, `moveq #8,D2` at `0x400db2ea` (immediate byte `0x400db2eb` = 08), `cmp.l D1,D2; shi D1b; and.l D1,D0` |
| second byte (+0xd3) | kept if `(u8)(b+1) < 7` (0xff, 0..5) else 0 | `move.b #7,D2b` (`143c 0007`) at `0x400db320` |
| other enum bytes (+0xd4..+0x128) | clamped or defaulted per field (for example `>4 -> 4`, `>100 -> 100`, `>0x3c -> 2`, `>0x16 -> 0xd`) | `FUN_400db24e` body |
| parameter slots | **copied raw, no range check** (s16, any value, negative too) | the copy loop at `0x400db2cc` |
| kit-level 16 x s16 at kit +0x1c | unsigned `> 0x7f00` -> `0x6400` | `FUN_400db734` |
| kit version != 4 or tail magic != `0xbacef00c` | kit rejected, replaced by default kit (`FUN_400e559c`), project continues | `FUN_400db734` entry |
| project head/tail magic or version != 5 | load fails (`FUN_400df7f6` returns 0); versions 1..4 are upgraded in place first by `FUN_400e0c26` (`FUN_400de3f0` v4 -> v5 is a plain copy) | `FUN_400df7f6`, `FUN_400e0c26` |
| file header gate (before the payload is read) | file type +0xd (project 1, bank 2, sound 3, kit 5), version +0x11 (project <= 5, bank <= 3, sound <= 3, kit <= 4, -1 invalid), payload size +0x19 <= buffer; else "Project/Sound/Kit version is newer than what the device supports" | `FUN_400e9922`, `FUN_400eb500`, `FUN_400ecaac`, `FUN_4002fa4e`, `FUN_4002faf0` |

So an unknown type or value never reaches a table index from a file, with one exception: the 0xff type is kept, and
what the dispatch (`FUN_400c8840`: `type > 6` -> entry 6, MANUAL SLICE, if the byte is passed as an unsigned long) and
other per-type tables do with 0xff was not traced **[O]**. The dispatch clamp and the `map::at` break described above
are therefore reachable only from the UI or from a patched loader, not from a stock file.

The permission gate `FUN_400da3b0(type, ?)` (bound 6, 7 x 4-byte table at `0x4020ecbe`) is **not** called by the
loader; it guards the UI paths (assign a machine, paste a sound, sound locks). Its callers in 1.16: `FUN_4002d438`,
`FUN_4003633c`, `KitActive::vfunc_30`, `FUN_4005066e`, `FUN_40051712`, `FUN_40051e42`, `FUN_400d681e` **[D]**.

**No integrity check over the file payload was found [D].** The CRC-32 routines (`FUN_4013e06c` poly `0xedb88320`,
`FUN_4013d858`) are used for the flash package header (`FUN_4002ccd0`, magic `MaGj`) and per packet by the MIDI file
transfer (`FUN_40126956`, "Invalid package checksum"); the Lz4 stream (de)compressor only reports stream errors.
The only validation in the record itself is the magic words and the version field. A project that a patched
firmware wrote is therefore not rejected by checksum **[O]** for the SysEx/RPC upload path beyond the packet CRC,
which the host computes.

### Round trip: patched -> stock

A patched firmware stores type 7 (and new parameter values in existing slots) with `FUN_400db930`, which copies the
type byte raw. A stock firmware loading that project, kit or sound:

1. file header and record versions are unchanged, so the file is accepted (**no "newer" error**) **[D]**;
2. every sound with type 7 loads as **type 0 (SAMPLE)**, silently, with its parameter slots copied raw into the SAMPLE
   machine's layout (so the SAMPLE parameters get whatever the new machine stored in the same slots) **[D]**;
3. other sounds, patterns, locks and the project are unaffected **[D]**;
4. if the stock firmware then saves, it writes type 0: **the type-7 information is lost** **[D]**;
5. parameter values outside the stock range load raw; whether the DSP or the UI clamps them on first use was not
   traced **[O]**.

### Consequences for patchkit

**[C] (2026-10-14) Type numbers are permanent, not "the k-th selected machine".** The first patchkit assigned `first + k` in canonical module order,
so a project saved with PLUCK as type 7 would load as another machine after the module set changed (a project stores only the
number, point 2 below). See "Stable machine type numbers" at the end of this file.

1. **One immediate decides whether the type survives load.** To keep type N-1 (the new highest id): raise the
   `moveq #8,D2` immediate to `N+1` (DT2 stock N = 7, new type 7 -> N = 8 -> byte `09`). Addresses of the immediate
   byte: DT2 1.16 `0x400db2eb` (function `FUN_400db24e`), 1.17 `0x400db44f` (function `0x400db3b2`); DN2 1.11
   `0x400dd287` (`FUN_400dd1ea`, stock `moveq #6`, 5 types), DN2 1.12 `0x400dd3eb` (function `0x400dd34e`). One patch
   covers project, kit, pool, bank and sound-file loads. Save needs no patch (raw byte copy). **[D]**
2. The same value is the whole persistence story for the type: no table, name list or version needs to change for
   storage, and the storage version must **not** be bumped (the header gates refuse newer versions on stock).
3. New parameter values persist only in an existing runtime index below 69 (it is in both permutation tables) and
   in a storage slot below 77. Anything beyond that (runtime 69/70, or a new index) needs both tables extended and
   both loop bounds changed (`0x8a` in `FUN_400db930`, `0x9a` in `FUN_400db24e`), the lock id bound `0x4d` and the
   `0x47` lock stride checked, and the 14 spare slots (77..90) used; the record size does not change. **[D]**
   DN2: 101 runtime entries, 107 slots, table at `0x401fd0b0`.
4. Parameter values are stored raw, so a new machine may use any s16 range, but kit-level values (16 x s16 at kit
   +0x1c) clamp at `0x7f00`. Do not put a new machine's values there.
5. The load does not call the permission gate. A patched build also needs the UI gate (`FUN_400da3b0` bound, table
   `0x4020ecbe` extended) so the user can assign the type; that is the `permit` part already described above, at the
   1.16 address.
6. On stock, a project saved by a patched firmware loads and plays with the new machine's tracks as SAMPLE; re-saving
   it on stock erases the type. A patchkit that wants stock-safe files has to say so in its notes (mark the project
   or keep type 0 on export).

Open **[O]**: the meaning of the byte at +0xd3; behaviour of type 0xff downstream; whether the DSP or UI clamps
out-of-range raw parameter values; `trackStorage_v2`; the project-level load error UI ("LOAD ERROR", "Reload Error");
a bounded emulator confirmation (load a hand-edited project with type 7 on stock, expect SAMPLE) was not run.

## core.machines: N added machine types on Digitakt II 1.17 (patchkit Phase 3)

Plan: `docs/plans/patchkit.md` (Phase 3). Code: `a1cf1df^:native/patchkit/src/machines.rs` (the Rust generator, removed in a1cf1df), `modules/core/adapters/machines`,
`tests/fixtures/modules/xslice`, `tests/fixtures/modules/machine-clone`, `c4d86e4e:tools/patchkit/pk_machine_rules.py`,
`tools/emulator/card_overlay.py`. One agent, emulator only, nothing flashed, not checked by a second agent: **[D]** unless
stated. A second agent verified **[V]** the static facts: the 24-site count, the list allocation sites (`pea 0x1c` and
`lea 0x1c(A0)` on DT2; `pea 0x14` on DN2 at `0x4004d8e0`), `FUN_400da23e` (typearg), the vector-191 type store, the 7th
clone site (`0x4002ec18`), the DN2 site bytes and the **[C]** about the rank shim; run-time results stay **[D]**.
Addresses are 1.17; 1.16 resolves the same rules (the function hashes are equal on both) and passed the same proof
(the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`)) and the harness cell with the smoke tour.

### What the framework does

Content modules declare `[[machine]] id, name, short, clone_of, position, fields (9 descriptor IDs), selector?` and
`requires = ["core/machines"]`. patchkit (Rust, so the browser patcher has it too) assigns the types `7, 8, ...` in
canonical module order (the order of `plan.modules`, so any selection order gives the same bytes: tested) and
generates, from the image being patched and from nothing stored in the repo:

| part | what is generated | sites |
|---|---|---|
| list | the 7 stock longs (copied) with the added types inserted at their `position`; the `pea` start and end operands repointed | `cf.machines.list` |
| list (new) | **the list builder allocates `pea 0x1c.w` bytes for seven types and sets the vector end with `lea 0x1c(A0)`**: both raised to 4 x total | `cf.machines.list_alloc`, list |
| dispatch | a shim at the entry of the descriptor dispatch: types above the stock ones index our descriptor array (stride 0x2c, name pointers to `std::string` reps with refcount -1, the nine IDs from the manifest); anything else falls into the stock code | `cf.machines.dispatch` |
| group, rank | bound raised to `6 + N`; the sort comparator's `std::map` initialiser gets `(type, position)` pairs for the longer list | `group`, `rank` |
| name, hint | the 12-byte display-name table copied with one row per type (long, short, hint = the clone's); three accessors repointed and their bound raised | `name.0..2` |
| permit, pertype | the permission longs and the per-type byte table copied with the clone's row appended; 1 + 6 bounds and table pointers | `permit`, `pertype.0..5` |
| clone | the `(type & ~2) == 4` test maps added types to their `clone_of` first (the `type == 6` tests for clones of MANUAL SLICE are gone: no machine clones it) | `clone.5` |
| typearg | `FUN_400da23e` (1.17 `0x400da3a2`: parameter mapping (mirror index, type, filter) -> code, a 7 x 10 table in bss, `type < 7`): an added type answers with **its own descriptor's field** for the engine slots 25..28 and 31..34 (0 where the field is empty: no MOD destination), and as its clone for the two slots without a field (29, 30: MANUAL SLICE's `Sample Bank` and `Sample Slot` rows, owner 6, mirror 29 and 30, listed first in its MOD destination list); **[C]** the first version mapped the type to `clone_of` and so gave an added machine the clone's defaults and rows, see "core.params" | `typearg.0` |
| persist | the sound loader's `(u8)(type + 1) < imm`: `imm = 8 + N` | `persist` |
| DSP | the machine type -> selector table (DM `0x2567c0`, words `0 1 2 3 4 0 5`) is copied into `dsp.l2` with one word per type (the clone's, or `selector`), and the literal `I3 = 0x2567c0` of `FUN_001c24e9` is repointed | `dsp.machines.selector_table` |

All 24 ColdFire sites are `[[anchor_rule]]` entries of `kind = "cf_func"` (new rule kind of `tools/patchkit/port_resolve.py`): the
cfdb `h_strict` hash of the containing function (address operands masked: no firmware bytes), the ordinal among
functions with that hash (two of the three name accessors share one), the offset and the length. `c4d86e4e:tools/patchkit/pk_machine_rules.py`
writes them from the 1.16 profile and checks them against 1.17 (every function matches by hash, one each). Each
generated patch checks the shape of the bytes it replaces (`moveq #6,D1; cmp.l ...` and so on) and fails with a message
otherwise. No patchkit op was added: the generator creates the objects and the patches itself; the "relocate a table"
need is `relocated_table`-style copying inside `generate()`.

Placement: **the shims and the tables both go to `cf.expansion`; `cf.flash_cave` holds the expansion loader only** (2026-10-13,
**[C]** the first version put the shims in the cave because the emulator fetched ColdFire instructions from the MAIN OS image
only, `UnsupportedGuestPc`; the native runtime executes from DDR and SRAM now, finding 07 "ColdFire code outside the MAIN
image"). Hardware executes from DDR (ACR0 covers it); the loader copies the code before the caches are on. Region use on
DT2 1.17 with `core/machines` + `test/xslice` + `test/machine-clone`: `cf.flash_cave` 56 of 2,108 bytes (the loader;
before: about 316 B of shim code for one type and about 80 B more per added type, 372 and 452 B with the loader, and the
cave capped the number of types), `cf.expansion` 744 B (code and tables). Evidence **[D]**: the machine proofs (the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`)) on
1.17 pass (one case fails, below), the smoke goldens of the XSLICE and XClone cells on 1.17 and 1.16 are unchanged
(frame hashes equal to the cave version), `tests/patchkit/test_patchkit_expansion.py` asserts the placement.

### Corrections to the earlier XSLICE work **[C]**

1. **[V]** `machinepatch.build_rank_shim` writes `move.l #imm,(8,A7)` (`2f7c imm 0008`). That instruction is **8 bytes; ColdFire
   instructions are at most 6**, our decoder says illegal and the core faults with exception vector 4 at the first
   `FUN_40052a08`. The old Unicorn-era runs accepted it (a generic m68k core). Any image made by `tools/patchkit/machinebuild.py`
   with the rank part (including the XSLICE `cdbd7726...` oracle) faults on a real ColdFire. The framework loads D0 first.
2. The list builder's `pea 0x1c.w` allocation (above): the old single added type overran it by four bytes (hidden by the
   allocator's rounding); a second type overran by eight and showed up as wrong names (GRID replaced by the new name,
   an `ERROR` entry) and corrupted lists.
3. The vector-191 frame builder (`0x4002ec18`, a `moveq #6,D1; mvs.b (0x990,A0),D0; cmp.l D0,D1; bne.w`) sends MANUAL
   SLICE's slice table (frame words `+0xee..+0xf8` of the track page) **only for type 6**. Without this seventh clone
   site an added clone sounds different from MANUAL SLICE (the audition ended after 0.5 s instead of playing the slice).
   It is not in the machinepatch profile.
4. The modulation destination picker (MOD page, DEST knob: "META, SRC, FLTR, ..." grouped by owner) lost the SRC group
   for any type above 6 until `FUN_400da23e` mapped the type. `SoundParameterSet::vfunc_21` (`0x4003ae02`, compares the
   track's type byte with a parameter's owner) is **not** on that path: restoring its original bytes in the patched image
   leaves the picker equal to stock; it is left unpatched **[O]**.

### The DSP side: binding contract

- **The type number reaches the DSP as the ColdFire's TX frame word `0x94 + 2t`** (finding 04), written from the track's
  type byte; measured on 1.17 with an XSLICE track: TX word `0x0007` (SRAM `0x800053dc`, high half of the long), and the
  DSP's copy `DM 0x255970 + 2t` (one 16-bit word per track; measured with XSLICE on track 2 and XClone on track 3: shorts `0 7 8`) holds 7 **after the first frame that follows a trig**
  (TRIG 1 or PLAY; before it the cache is 0). The DSP uses it only as a change detector and through the selector table.
- **The DSP indexes the selector table `0x2567c0` with the type** (`FUN_001c24e9`: `S2 = DM(I3, M2)`), stores the word as
  the per-track selector (`track_base + 0x4c`) that picks the render path (entries of `0x8055c840`). Stock: words 0..6 =
  `0 1 2 3 4 0 5`; **word 7 is the copy-gate flag at `0x2567dc`, 1 in the loader image and 0 after the first frame
(finding 06) [C], and words 8 and up are loader data pointers**, so an added type
  would read a pointer as its selector. The framework therefore relocates the table into `dsp.l2` (stream block, SHARC
  data) with one word per added type: the clone's selector by default (a MANUAL SLICE clone gets 5 and sounds exactly like
  it), or `selector = n` in the manifest for a new render path (the DSP lane maps `n` to its code).
- **A DSP module binds to its type through the extern symbol `machines.<id>.type`**: in its manifest
  `[object.externs] _my_type = "machines.<id>.type"`; patchkit resolves it to the assigned number (a plain absolute value,
  `sharc.abs32` or `cf.32` relocations) after the canonical assignment, so a DSP module's bytes change with the module
  set exactly as its type does. Machine ids are claims (`machine.id.<id>`, `machine.type.<n>`); two modules with one id
  fail with a message. The module that supplies the machine requires `core/machines`; a DSP-only module that wants a
  type it did not declare needs a `[[machine]]` entry too (it may be the whole module).
- For a **new render path** (not a clone): choose `selector = n` for an entry the DSP lane adds to its own dispatch
  (the array `0x8055c840` holds 15 entries; `n >= 6` is unused by stock types, finding 06), read the type from
  `DM 0x255970 + 2t` in its voice hook, and leave `clone_of` at the stock type whose descriptor and per-type rows the
  ColdFire side should copy.

### Evidence (the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`), DT2 1.17, health asserted after every input)

| check | result |
|---|---|
| FUNC+SRC lists XSLICE (8th entry, after MIDI) and, with a second module, XClone after it | frames under `out/runs/machines-proof/` |
| YES commits: track object type byte (`0x4265338e`) | 7 (XSLICE), 8 (XClone); stock SLICE 6 |
| TX frame word and DSP cache after an audition | 7 and 7 (XSLICE), 8 and 8 (XClone) |
| audition PCM of XSLICE / XClone / patched SLICE vs stock SLICE | **bit-identical** (0 differing samples; patched SLICE needs a one-block capture shift on the first image) |
| sequencer (TRIG 1, PLAY, 2 s before STOP) | bit-identical for both clones |
| MOD DEST picker, 14 frames over the SRC group | equal to stock SLICE for both clones |
| patched: save project with XSLICE, set track to SAMPLE, load the project | type 7 again (track object, TX, DSP) |
| control: the same image with the persistence immediate put back to `8` | type 0 after the load (so the immediate is what keeps it) |
| the saved project's sound records | type 7 in 2 records (`card.overlay` export, `tools/emulator/card_overlay.py`) |
| the saved card laid over the stock firmware's card; STOCK loads the project | track 1 is **SAMPLE** (type 0), no fault, halt or stall, TRIG 1 sounds |
| `test/xslice` + `test/machine-clone` in either order | the same `.syx` bytes; types 7 and 8 |

The proof needed one emulator addition: `card.overlay {path}` (packages/emulator/{card,machine,boot,control}: the guest's writes to
the eMMC as sparse sectors) so a card with a patched firmware's project can be mounted by another firmware.

- **[O]** Part d of the proof, `XClone`'s MOD destination frames equal to MANUAL SLICE's, **fails on 1.17 and 1.16, and on 1.17 with the cave
  placement and the old runtime too** (checked at `ff5a1a5`: same 14-frame comparison, XSLICE passes, XClone differs; PCM,
  type, persistence and sequencer checks of XClone pass). Not caused by the placement or the runtime change; which frame
  differs was not examined.

### Open **[O]**

- Hardware: never flashed. The shims now run from DDR (`cf.expansion`), where the loader copies them at reset; the DSP selector
  block lies in a region the real SHARC boot ROM has not been shown to accept (finding 06).
- DN2: no support (below).
- Other tests of the clone's type may exist that no run exercised (p-lock edits of the machine's own parameters,
  copy/paste of a sound between tracks, kit load with mixed types). Parameter-knob effects on the DSP were not shown:
  the encoder turns on the SRC page moved the screen state (the SLICE/NOTE switch) but no turn changed a parameter value
  in the track object or the frame page in these runs, for stock SLICE either.
- A PCM difference of about 1e-3 appears for XClone (not XSLICE) in the release after STOP in one run; with the capture
  stopped before STOP both are bit-identical. Attributed to capture timing, not measured.
- Types above 8 (a third module) are untested; the framework allows 32.
- Kits saved on patched firmware and loaded on stock lose the type (they become SAMPLE; re-saving erases it): said in the
  gallery text of the module.

## core.params: parameter descriptors for added machines (patchkit) **[D]**

Plan: `docs/plans/patchkit.md` (Phase 3 and 4.2 results). Code: `a1cf1df^:native/patchkit/src/params.rs` (with `a1cf1df^:native/patchkit/tests/params.rs`, 5
integration tests on the real ports and images, 3 unit tests), `[[param]]` in `module.rs`, `modules/core/adapters/params`,
`tools/patchkit/port_resolve.py` (three rule kinds), `modules/machines/pluck`, `tools/patchkit/pk_proof_pluck.py`. One agent, emulator only,
nothing flashed, not checked by a second agent: **[D]** unless stated. A second agent verified **[V]** the static site
counts (56 reference + 55 bound sites; the 3 `bne` and 1 `beq` compares are left alone) and the T1/T2 shapes (the T2
accessor `0x400c892e` begins `move.l 4(sp),D0; cmpi.l #0x113,D0`); the coverage tables stay **[D]**. Addresses are 1.17
(1.16 resolves the same rules).

### What a parameter code indexes

A parameter code (the nine ID fields of a machine descriptor, `0 .. 0x112`) is used as an index into several structures,
each behind its own inlined clamp `code < 0x113 ? code : 0` **[D]**:

| structure | where | readers |
|---|---|---|
| parameter table T1: 275 rows of 60 bytes (finding 02 above) | rodata `0x4020f59c` (1.16 `0x4020f18c`) | about 50 functions (small accessors from `0x400da03c`, the page views, the `ParameterSet` virtuals): **56 absolute references** to the table, **55 clamp bounds** (`cmpi.l #0x113,Dn` followed by `scs`/`scc`, `cmpi.l #0x112,Dn` followed by `bhi`/`bls`; also `cmpa.l`) |
| value-class objects T2: 275 objects of 0x44 bytes (the type class, two numbers, three `std::function`s; heap pointers) | bss `0x4293c040`, filled at boot by the static initialiser `0x401be1be` | one accessor `0x400c892e`, 15 callers; rows of one value class are equal except for heap addresses (SAMPLE `STRT` and `LEN` equal, `TUNE`, `PLAY` and `LEV` differ in class) |
| owner-keyed lookup maps built from T1 at boot by `0x400da84e` | bss: `[owner*10 + mirror - 25]` array, `std::map`s keyed by the row's CC number (`+0x18`) and by `+0x1c` | `0x400da3a2` (index, type, filter) -> code; `0x400dab9e` (CC -> code) and `0x400dac80` (the `+0x1c` key -> code) |

Three loops visit **every** code and end on `bne`, not on a clamp: the defaults refresh `0x4004fefc` (for each code it writes the
formatted default into the sound array at the row's mirror index, so for aliased mirror indexes the **last row in table order
wins**), the map builder `0x400da84e`, and an owner enumeration `0x4003d942`. They are the reason a new row must not simply be
appended and counted everywhere: raised, the defaults refresh would overwrite the stock defaults of the SRC slots with the new
rows' values for every sound. The framework raises only the clamps.

Row fields learned here (T1): `+0x18` high i16 = `0x10 + n` and `+0x1c` = `0x80 + n` for descriptor field `n` (TUNE 0x10/0x80
... LEV 0x17/0x87; SAMP has -1 and 0x83): the MIDI CC number (CC 16..23 on the Digitakt) and a per-machine key; `+0x20` and `+0x24` as
before **[O]**. A row is a complete description for the SRC page: the page shows `+0x28` while the knob is touched
("Brightness=88") and `+0x30` as the label.

### What the framework does

Content modules declare `[[param]] id, name, short, template, slot, min, max, default, formatter_from?` and
`requires = ["core/params"]`; a machine's `fields` may name them (`"pluck_decay"`). patchkit assigns the codes `275, 276, ...` in
canonical module order (selection order does not change the bytes: tested) and generates **at patch time, from the image**:

| part | what is generated |
|---|---|
| table | T1 copied to `cf.expansion` (16,500 bytes) with one 60-byte row per added parameter: the template row (owner, flag, kind, masks, formatter, +0x38 kept), then the mirror index of `slot` (25, 26, 27, 28, 31, 32, 33, 34), `min/max/default`, `+0x18` high i16 `0x10 + slot`, `+0x1c = 0x80 + slot`, the long and short name pointers into strings placed after the table, and optionally another row's formatter pointer |
| references | every 2-aligned big-endian word of the code below the table that points into `[T1, T1 + 275 * 60)` is repointed (56 on both firmwares); the table's end address is not one (another table follows it) |
| bounds | every clamp of the shapes above is raised by the number of added rows (54: the 55th lies in the accessor below). Compares that end loops (`bne`/`beq`) are left alone |
| T2 | the accessor `0x400c892e` begins `move.l 4(sp),D0; cmpi.l #0x113,D0`; a 10-byte `jmp` to a 54-byte shim in the flash cave maps an added code to its **template's** code (a `u32` map in `cf.expansion`), so it answers with the template's object. Sharing is safe only if the object is constant after boot: **[O]** not shown in general; the four rows of PLUCK use the level class (`LEV`'s object) and behave |
| maps | not touched: the owner maps and the CC/NRPN lookups never see the new rows |

The scan is part of the port: `tools/patchkit/port_resolve.py` records the **count and a sha256 of the site list** in the anchor
`cf.params.sites` (kind `sites`), the table in `cf.params.t1` (found by the 36-byte signature of its reader, finding 02 above) and
the accessor in `cf.params.t2` (`cf_func`); patchkit runs the same scan and refuses an image whose sites differ. Both firmwares: 111
sites (56 + 55). Cost: 16,832 bytes of expansion for four rows (16,500 + 60 K + names + the `u32` map), 54 bytes of flash cave (the cave holds 2,108; core/machines
(216 B), core/params and the loader use 328 together).

### Corrections to core.machines **[C]**

1. The type-argument shim of `FUN_400da3a2` (above) now returns the machine's **own** descriptor field instead of the clone's code (and the clone's for slots 29 and 30; a first version that returned 0 there dropped `Sample Slot` and `Sample Bank` from XSLICE's MOD list, caught by the machine proofs).
   With the old mapping a new machine got the clone's defaults when selected: selecting PLUCK gave TUNE 0x4000, PLAY 0x300 in slot 1
   (DECAY would be 0.02: a click), BRIGHT 0, COLOR 0x7800. Measured before and after (`tools/patchkit/pk_proof_pluck.py`, part a).
2. After YES in the machine list the track's sound array reads **zero** in the engine slots until the next trig; the values (the
   defaults) appear at the first trig or sync. Stock SLICE and SAMPLE behave the same. A read back needs a trig first.
3. The knob problem of the machine proof (no turn changed a value): not a bug. `input.turn` numbers encoders from 1 (A..H) as the
   harness does. The firmware's encoder accelerates by the time between events: **one or two slow events change nothing** (a
   value moves about one raw step per slow event), 30 events of +4 detents 2 ms apart move TUNE `0x4000 -> 0x401e`, STRT
   `0 -> 0x1e`, LEV `0x6400 -> 0x6e00`. The harness now has `Session.turn_burst`, `turn_to` (turn until a slot reads a value) and
   `mirror`/`tx_word` read-backs. Proven on stock SAMPLE: encoders A, E, F, G, H change slots 25, 31, 32, 33, 34 in the Sound object
   (`*(0x8000470c + 4t) + 0x14 + 2 * idx`) **and** in the TX frame words `0x80005348 + 0xda + 0x60 t + 2 (idx - 25)`; B (PLAY), C (no row)
   and D (SAMP, a mode) do not change a value.
4. On the SRC page the TRIG keys select the sample slot (a trig key tapped there showed `Oneshot:PRESET 5`), not steps: place
   steps on the main page first.

### Coverage of a new machine's parameters

| subject | result |
|---|---|
| SRC page names, values, ranges | works: the page shows the row's long name while the knob is touched and the value 0..127 from `min/max` ([D], frames `out/runs/pluck-proof/`); the descriptor chain was also read back from the running firmware (field -> row -> name pointers) |
| defaults on selection | works (needs the typearg change above) |
| persistence (project save and load) | works **positionally**: the engine slots are the stock mirror indexes 25..34, persisted by the stock permutation tables; no storage change. Measured: DECAY and BRIGHT edited, project saved, track set to SAMPLE, project loaded: type 7 and all six slot values back; the card's sound records carry type 7 (2 records); the card laid over the **stock** firmware's: the track loads as SAMPLE, no fault. A new row's `slot` must be one of the eight fields (a new mirror index would need the permutation tables and bounds extended, finding 02 "Persistence") |
| modulation destination picker (MOD page, DEST) | works: the SRC group lists the machine's own names (`Tune, Sample Level, Decay, Pick position, Brightness ...` for PLUCK) |
| p-lock | **not shown in the emulator** [O]. Locks are stored by mirror index (lock id <= 76, `lock[track * 0x47 + idx]`, finding 02), so a new row in an existing slot should lock like the stock one; the hold-trig-and-turn gesture shows `Sample Level=90` on stock but no effect was seen in the TX frame or the audio even for stock SAMPLE, so the gesture is not harnessed |
| MIDI CC and NRPN | **ignored on added types** (static): `0x400dab9e` and `0x400dac80` take the machine map only for `type < 7` (an added type falls to the filter and common arrays, no overrun), so CC 16..23 do nothing on a PLUCK track. To fix it needs a wrapper around the two lookups that maps the type to `clone_of` for the lookup and translates the returned code to the machine's own row (return address kept across the call), or the maps extended. Not built |
| kit save/load, copy and paste of a sound | the persistence immediate and the permission rows are core/machines'; not exercised separately |

### PLUCK on DT2 (`tools/patchkit/pk_proof_pluck.py`, 1.17 and 1.16)

`digikit/pluck` clones SAMPLE (so its permission, per-type byte and DSP selector are SAMPLE's and the voice hook takes over in the
sample renderer) with descriptor fields `[TUNE 0xca, DECAY, PICK, 0, BRIGHT, COLOR, 0, LEV 0xd1, 0x0a]`; DECAY, PICK, BRIGHT, COLOR are
rows copied from `LEV` (a plain 0..127 value class and formatter) with range 0..0x7f00 and defaults 0x4000, 0x2000, 0x5000, 0x2800. The
DSP routine registers `machines.pluck.type` (the same symbol mechanism as the extern) in the `core/dsp-voice` table. Evidence on the
emulator (health asserted after every input; the numbers below are 1.17):

| check | result |
|---|---|
| FUNC+SRC lists PLUCK as the 8th entry; YES commits type 7 | track object type 7, TX frame word and DSP cache 7 |
| defaults after the first trig | slots 25, 26, 27, 31, 32, 34 = `0x4000 0x4000 0x2000 0x5000 0x2800 0x6400`, equal in the TX frame |
| encoders A, B, C, E, F, H change slots 25, 26, 27, 31, 32, 34; D and G nothing | 6 of 6, in the Sound object and the TX frame |
| fundamental against the pitch word sent by the ColdFire (TRIG 1 = note 60, keyboard keys 61, 63, 66, 67, 72) | 0.0000 to -0.0020 cents (261.626, 277.182, 311.127, 369.994, 391.995, 523.251 Hz) |
| DECAY (BRIGHT at the top) | T60 0.26 s at 0x2000, 1.00 s at 0x4000 (requested 1.02 s), 1.53 s at 0x5000 |
| BRIGHT | energy above 3 kHz relative to the body 0.2 s into the note: -26.4 dB at 0x0800, +6.7 dB at the top (+33 dB) |
| COLOR | excitation HF/LF 12.1 dB at 0, -5.1 dB at the top (17 dB lower) |
| PICK | see the evidence file: the notch at the 4th and 8th harmonic at PICK = 1/2 |
| sequencer | four plucks in 2.3 s (onsets by the energy above the second difference), first at note 60 |
| save and load | type 7, DECAY 0x2200 and BRIGHT 0x5e00 back; stock loads the project as SAMPLE |
| MOD destination list | the SRC group shows the five PLUCK names |

### Open **[O]**

- Hardware: never flashed (as core.machines). The new table lives in `cf.expansion`, read like any RAM data.
- MIDI CC/NRPN and p-lock as above; kit and sound copy between tracks and between PLUCK and other machines not exercised.
- A fourth row class for T2 sharing and rows whose class differs from the template's (a discrete knob as a new row) were not tried.
- The new rows are outside the owner maps: the picker and CC/NRPN logic that enumerate by owner will not list them on any type but through the descriptor.
- DN2: not supported (a different table, 321 rows, an FM engine; the parameter-page layer is unexplored, see the assessment above).

## core.machines on Digitone II 1.12 (patchkit, lane fastweb) **[D]**

Emulator only, nothing flashed, one agent, not checked by a second one: **[D]** unless marked. Code:
`a1cf1df^:native/patchkit/src/machines/dn2.rs` (the Digitone II part of `machines.rs`: the Digitakt code paths for rank,
per-type byte, clone tests and the DSP selector table are skipped on `dn2`), rules `c4d86e4e:tools/patchkit/pk_machine_rules_dn2.py`
(`cf_func` hashes, 1.12 reference, 1.11 resolves every one), module `tests/fixtures/modules/dn2-clone` (a clone of FM TONE as
type 5), proof the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`), tests in `a1cf1df^:native/patchkit/tests/machines.rs`
(DN2 1.12 and 1.11 patch; the Digitakt output is byte-identical to the previous patchkit for `core/machines` +
`test/xslice` + `test/machine-clone` on 1.16 and 1.17). Addresses are 1.12; 1.11 is the same by function hash.

**What is shared with the Digitakt framework, unchanged** (sites verified by byte shape): descriptor dispatch
(`0x400c25f2`, `moveq #4,D1; move.l 4(sp),D0`; our descriptors carry the nine IDs `0 0 0 0 0 0 0 0 10`: the stock DN2
descriptors of all five machines are exactly that, built at start-up by `FUN_400c2c4e`), list builder (`pea 0x14.w`
allocation, `lea 0x14(A0)`, the two `pea` of the list source), the three name accessors (`0x400dc496`, `0x400dc4bc`,
`0x400dc4e2`; the last two are not Ghidra functions; the table `0x401f7c00` is read in the shifted layout
`{name, abbrev, hint}` from `+4`, hints null on every row), the permission check (`0x400dc2fe`, bound 4, table
`0x401f7d42`), the persistence immediate (`moveq #6,D2` at `0x400dd3ea` in the sound loader `0x400dd34e`: `(u8)(type+1) < 6`;
raised to `6 + N`; the sound record's type byte is at `+0xf4`, saved by `FUN_400dd80a` raw).

**What is different on the Digitone II**

| part | Digitone II 1.12 |
|---|---|
| group helper (`0x40059274`) | types 0..3 group 1, 4 group 2, else 0 (no `type == 6` form): a shim at `0x4005927a` sends types 5..4+N to group 1, so a clone is listed with the synth machines (put it at `position = 4`, before MIDI) |
| sort comparator, per-type byte table, clone tests, DSP selector table | none exist: skipped |
| per-type page tables | `FUN_400c2636(type)` (page count) and `FUN_400c2652(type, page)` (the page descriptor, stride 0x2c, table at `0x42433ad4` stride 0x10) bound the type by 4 and fall back to MIDI's row; `FUN_400dc18e(param, type, sub)` maps (parameter slot 25..64, type) to a parameter code through a 5 x 40 table at `0x42c65d18`. A shim at each maps an added type to its `clone_of` right after the stack argument is loaded (`cf.machines.typemap.0..2`) |
| the type reader `FUN_4004b7f2` | 41 callers (pages, parameter code, clipboard) test its result against FM TONE's number 0 and so on (the SYN page 2 of FM TONE draws the algorithm diagram and the envelope views only for type 0). A shim at its `mvs.b (0xde,A0),D0` returns the clone's original for every caller except the identity code: list stepping (`0x4004d9a8`, `0x4004da90`), preset receive, name of the machine, clear track, sound snapshots, kit events and the callers of `FUN_4003134e` (`(project, track) -> type`, a tail jump into the reader: the machine list and selection views, names, kit events); those get the own type by return address or by a verbatim copy of the reader |
| `SoundParameterSet::vfunc_21` (`0x40036bfa`) | `type == the parameter's owner machine`: the type loaded at `0x40036c24` is mapped too |
| the DSP | the track's type reaches the DSP as TX frame word `0x94 + 2t`, as on the Digitakt (`0x20000` for FM DRUM after a TRIG, high half = track 1); written by the vector-191 handler at `0x4002757c` (`move.b (0xd8c,A0),D0b` from the SRAM copy `0x8000487c + 0x99 t` of the sound at `+0xde`). The FM engine knows types 0..4 and treats a type above 4 like MIDI (nothing rendered), so a clone without a voice of its own is **sent as its original** (shim at that load; `own_dsp = true` in the manifest keeps the own number for a machine a registered voice routine renders) |

**Evidence** (the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`); FM CLONE = test/dn2-clone):

| check | result |
|---|---|
| FUNC+SRC lists FM CLONE (5th, before MIDI, synth group); YES commits; the reopened menu marks it as the track's machine | row 4 |
| audio mirror after a TRIG / TX frame type word | 5 / 0 |
| audition of FM CLONE vs stock FM TONE | **bit-identical** (48,033 active samples) |
| SYN, FLTR, AMP pages | frame hashes equal to FM TONE's (title "FM Tone", the algorithm diagram, the envelope views) |
| the same encoder turns (ENC A +20, B -15, E +25) | PCM equal to stock FM TONE with the same turns; the turns change the sound |
| save project, set the track to FM TONE, load the project | FM CLONE is current again; PCM equal to the turned stock sound; the card writes hold type 5 in 2 sound records (byte `0xf4`) |
| the stock firmware loads the same project (same daemon, `firmware.load` of the stock image) | no fault; track 1 is FM TONE; PCM equal to stock FM TONE with the same turns (the parameters persisted, the unknown type became 0) |
| control: the persistence immediate put back to 6 | the loaded track is FM TONE |
| sequencer (RECORD, PLAY, TRIG 1 held; stop; play back) | PCM equal to stock FM TONE |
| harness smoke tour (boot, track, pattern, play/stop, machine menu, save/reload, stress) with the module set | 33 checks pass |

**Limits and open [O]**

- The clone has FM TONE's page layout, parameters, names of pages and sound; its own name is shown in the machine list
  and menus only. The page title reads "FM Tone" (it comes from the page descriptors, which the clone shares).
- A new machine with its **own parameter pages** (not FM TONE's) needs its own rows in the three per-type tables
  (`0x42433ad4`, `0x42433b24...`, `0x42c65d18`, built at start-up in bss by `FUN_400c2c4e` and a neighbour): the
  descriptor rows are 9 parameter slot IDs (10 ends a list) and the 5 x 40 table maps slots to parameter codes. Not
  built: the tables are RAM structures made by code, so growing them means a shim after their builders (as the Digitakt
  list builder shim) or relocating them with their eight readers; est. 1 to 2 days, **[O]**.
- The reader's callers that were not classified (about 25) get the clone's original; a behaviour that needs the own
  type there (copy and paste of a sound between tracks keeps the type through the snapshot; not run) is open.
- Changing the machine twice in one emulator session does not update the audio mirror (`0x8000487c`) the second time
  in stock firmware either (the type posted by the UI thread is consumed once); every proof starts from `restart`.
  Cause not found; the UI and the saved project follow the second change **[O]**.
- Hardware: never flashed; the shims run from the flash cave (212 bytes + 40 per added type).

## DN2 parameter pages: what an added machine's own SYN pages need (wave 7, lane fastweb) **[D]**

Digitone II 1.12 (1.11 by function hash and by the table scan). One agent, static reading plus emulator probes
(`tools/coldfire/dn2_probe.py`, `tools/coldfire/dn2_pages.py`, `tools/coldfire/cf_wordscan.py`, `c4d86e4e:tools/patchkit/pk_params_rules_dn2.py`); not checked by a
second agent. Nothing flashed. **[C]** to "core.machines on Digitone II 1.12" above where stated.

### The layers, from the knob to the engine word

| layer | Digitone II 1.12 | evidence |
|---|---|---|
| T1, the parameter descriptor table | rodata `0x401f83a4`, **321 rows** of `0x3c` bytes: the Digitakt's layout (`+0` owner, `+4` sound-array slot, `+8/+0xc/+0x10` min/max/default, `+0x14` flag, `+0x18` hi i16 = MIDI CC (40..77 on FM TONE) or -1, `+0x1c` a per-owner key, `+0x28` long name, `+0x2c` owner name, `+0x30` short name, `+0x34` formatter, `+0x38` constant); `tools/coldfire/dn2_pages.py --params` dumps it | rows read from a snapshot, 5 machines |
| page table | bss `0x42433ad4`, rows of 16 bytes `{count, begin, end, cap}` for types 0..4 (FM TONE 4 pages, WAVETONE 3, FM DRUM 4, SWARMER 1, MIDI 1); the heap vectors hold **page descriptors of `0x2c` bytes**: two `std::string` pointers (`SYN1` / `FM Tone`, the second is the title shown) and **nine parameter codes** (T1 row indices; `10` = Track Level ends a page; `0` = no knob) | snapshot dump; page 1 of FM TONE: `0xd4 .. 0xdb` |
| the slot to code table | bss `0x42c65d18`: **5 x 40 words**, `[type * 40 + slot - 25]` = the code whose T1 row has that owner and slot (FM TONE uses all 40 slots 25..64, WAVETONE 25..49, FM DRUM 25..54, SWARMER 25..32); `0x42c65cd0` is the filter table (6 types x 3, slots `0x42..0x44`), `0x42c65b3c` the global one | `FUN_400dc18e(slot, type, sub)` disassembly; `0x42c65d18` is read only there and in its builder |
| T2, the value-class objects | bss `0x4243425c`, stride `0x44`, one per code; accessor `FUN_400c257c(code)` (`cmpi.l #0x141,D0`, 15 callers); `FUN_400c25a0(code, ..)` calls the row's formatter | disassembly |
| the sound array | per track, slots 25..64 (FM TONE: all 40); persisted by the stock permutation | unchanged from the Digitakt layer |

**Who builds them.** `FUN_400c2c4e` (35,712 bytes, no caller: a static initialiser) builds the page vectors, the machine
descriptors (`0x42433b24 ..`, `0x2c` bytes, nine IDs `0 .. 0 10`), the filter pages and T2 in bss/heap at start-up;
`FUN_400dc634` (called from `FUN_400bb3c0`) clears six bss tables and fills the slot tables from T1 by one loop over
`0x141` rows (the owner and slot of each row: owner `< 5` = machine, `5..10` = filter). The page table is read only by
`FUN_400c2636(type)` (page count; callers `MultiSourcePageView::vfunc_32/33`, `FUN_400173ca`) and
`FUN_400c2652(type, page)` (descriptor; callers `MultiSourcePageView::vfunc_47/19`, `FUN_40045c82`, `FUN_4004610a`,
`FUN_4005c4de`, `FUN_40064e76`); both bound the type by 4 and fall back to MIDI's descriptor. The slot table is read only
by `FUN_400dc18e` (callers `SoundParameterSet::vfunc_20`, `FUN_4003f6a0`, `SoundParamChangedInfo` x2, `FUN_400e7eb0`,
`FUN_40011f66`; `cf_wordscan.py` over the image finds no other absolute reference to either table).

**How a page slot binds to a descriptor [D, emulator].** The page descriptor's field n is a T1 row index; the row gives
the sound-array slot, range, name and formatter. Proof: the heap descriptor of FM TONE's second page was edited in the
running emulator (`dn2_probe.py pagepoke --page 1`: field 0 `0xd4` (`A Env Attack`, slot 35) -> `0xe3` (`Pitch All`, slot 50)):
the page shows "Pitch All=10" on knob A and 30 x +4 detents move the TX frame halfword `0x10c` (slot 50), not `0xee`.
Nothing else is keyed by the page.

**What the widgets are.** `MultiSourcePageView::vfunc_4` (the draw) switches on the type reader and the page index: type 0
(FM TONE: algorithm and envelope views), 1 (WAVETONE: special cases keyed by the hard-coded codes `0xf7 0xef 0xfb 0xf3`),
2 (FM DRUM: pages 8 and 10), 3 (SWARMER: codes `0x126 0x127`); **every other value, and every unmatched page, goes to
`FUN_40017428`, the generic eight-knob page** (name, value from the row's formatter). So an added machine gets the
generic look when the draw functions see its own type (>= 5), or the clone's widgets when they see the clone's.
Screens: `out/runs/dn2-shots/` (`dn2_probe.py shots`).

### What a framework must add for own pages (the delta to core.machines and core.params)

1. **Rows (core.params on DN2).** One 60-byte T1 row per new parameter (code `321 + k`, owner = the clone's type, slot in
   25..64 (40 slots per machine, the frame carries 25..65), name/short/formatter/range; copy a template row of the same
   value class), the table copied to the expansion region, **56 references repointed and 55 clamp bounds raised**
   (`cmpi.l #0x141` followed by `scs/scc/bhi/bls`; compares that end a loop with `bne` stay at 321, as on the Digitakt:
   `FUN_400dc634` at `0x400dc954`, `FUN_400394be` at `0x4003950a`, `FUN_4004b0b4` at `0x4004b17a`; the scan skips them). The Digitakt scan (`params::scan`, `port_resolve.py cf_params_t1` / `cf_params_sites`)
   gives **the same numbers on the Digitone II (321 rows, 56 + 55 sites; 1.11 identical)** and needs no change but the
   device filter. T2: the accessor `FUN_400c257c` needs the Digitakt's shim (an added code answers with its template's
   object; same caveat: [O] that the shared object is constant after boot). MIDI CC / NRPN maps (`0x42c67a84 ..` keyed
   through `FUN_401af0c8`) ignore added rows as on the Digitakt.
2. **A slot table row per added type**, 40 words (`slot - 25 -> code`, 0 for unused), read by the existing shim at
   `FUN_400dc18e` (`cf.machines.typemap.2`, `0x400dc196`, 8 bytes): for a raw type >= `machines.first` and a slot
   `0x19..0x40` return the row's word instead of mapping the type to the clone. Defaults on selection come from this
   path (`SoundParameterSet::vfunc_20` reads the raw type byte `+0xde`).
3. **A page record per added type**: `{count, begin}` and `count` page descriptors (`0x2c` bytes: two `std::string`
   pointers to representations with refcount -1, nine codes), read by the shims at `FUN_400c2636` / `FUN_400c2652`
   (`typemap.0` at `0x400c2636`, `typemap.1` at `0x400c2656`): for a type >= first return the own count / descriptor
   (bounds as the stock: a page outside the count falls back to MIDI's row `0x42433bd4`). All data is constant: no
   heap, no builder hook.
4. **The type the page callers see.** Today the reader (`FUN_4004b7f2`, 41 callers) returns the clone's original to
   every caller but a list of identity ones, so the page functions would receive the clone's type. For own pages the
   callers of the page functions must get the **own** type: `MultiSourcePageView::vfunc_47` (return addresses
   `0x4001688a`, `0x400168ac`), `vfunc_32` (`0x4001690a`, `0x40016926`), `vfunc_33` (`0x40016996`, `0x400169b6`),
   `FUN_400173ca` (`0x400173fe`, `0x40017414`), `vfunc_19` (`0x400177dc`); `FUN_40064e76` already reaches the identity tail
   `FUN_4003134e`; `FUN_40045c82`, `FUN_4004610a` and `FUN_4005c4de` get their type from elsewhere (check each: **[O]**).
   For the **generic** look the draw callers get the own type too: `vfunc_4` (`0x4001815c`, `0x40018176`), `vfunc_45`
   (`0x40016a3e`, `0x40016a5c`), `vfunc_17` (`0x40017086`, `0x400170a0`), `FUN_40015f90` (`0x40015fb6`), `FUN_40015fec`
   (`0x40016012`): about 17 more identity sites in the same list as `cf.machines.reader_call.*`.
5. **Not needed:** the machine descriptor (nine zeros), the persistence immediate (already raised), any change to the sound
   array, the DSP frame (slots 25..65 are carried as they are).

Anchors (proposed; `c4d86e4e:tools/patchkit/pk_params_rules_dn2.py` prints and `--check dn2-1.11` verifies them; the page, slot and reader
sites above are the existing `cf.machines.typemap.*` anchors plus new `cf.machines.reader_call.9..` byte-shape ones):

```toml
[[claim]]
resource = "cf.params.sites"

[[anchor_rule]]            # T1: 0x401f83a4, 321 rows (1.11: 0x401f7f94)
name = "cf.params.t1"
devices = ["dn2"]
cpu = "cf"
kind = "cf_params_t1"
mark = "D"

[[anchor_rule]]            # 56 references + 55 clamp bounds, digest 463d362c04d4050c (1.12), 4efbd2b0facd4ab0 (1.11)
name = "cf.params.sites"
devices = ["dn2"]
cpu = "cf"
kind = "cf_params_sites"
mark = "D"

[[anchor_rule]]            # FUN_400c257c: move.l 4(sp),D0; cmpi.l #0x141,D0 (T2 accessor)
name = "cf.params.t2"
devices = ["dn2"]
cpu = "cf"
kind = "cf_func"
hash = "ad9a012d7c22efe3de4430746f3ad4cf"
offset = 0x0
len = 10
mark = "D"
```

`cf.params.fmt` (`FUN_400c25a0`, hash `7fe71c352cb62e7f7f486c40226d021e`) and `cf.params.slotbuild` (`FUN_400dc634`, hash
`2ecb4ffb3045ee944fc97fd5a593b0f9`) are informational rules (a bound user and the start-up builder); a module that
only adds rows does not need them. Estimate for own pages on DN2 as modules: **1 to 2 days** (the params part is the
Digitakt's, the page and slot shims are about 120 bytes of ColdFire, the identity-site list is mechanical); confidence
about 70 % that every page shows own names, values and defaults, about 50 % that the generic look has no seam (a
view that keys on the type elsewhere).

### Where the engine parameter words are, and what reaches the DSP **[C]**

The TX frame (`0x80005e60`, sent by the vector-191 handler `0x40025e36`) has, for **voice slot v** (not track), a block of
`0x92` bytes at `0xda + 0x92 v`; the halfword of sound-array slot `s` (25..65) is at `0xda + 0x92 v + 2 (s - 25)`
(`dn2_probe.py knobs`: SYN pages 1..4, encoders A..H, 30 x +4 detents at 2 ms: SYN1 knob A moves frame `0xda` (slot 25),
SYN2 `0xee` (35), SYN3 `0xfe` (43), SYN4 `0x11c` (58), the others in order; values lag by the smoother).
The DSP's copy at `0x25c48c` is byte-for-byte the same (73 of 73 halfwords equal for slots 0 .. 9, `dn2_probe.py dspblock`).
**[C]** finding 04 and the plan said "per track": the block belongs to the voice slot; the ColdFire writes it from the
track that owns the voice at the note-on and keeps updating it while the voice lives (a knob turned while the key is held
moves the slot's word; before any note no block changes). See finding 06, "DN2 voice slots", for the slot to track map.

## core.machines, core.params and core.dsp-voice as modules (patchkit 3b.2/3b.3, lane b) **[D]**

**[C]** to the code references above: `a1cf1df^:native/patchkit/src/machines.rs`, `machines/dn2.rs`, `params.rs`, their tests, and the
manifest keys `[[machine]]`, `[[param]]`, `[[table]]`, `op = "register"` and `op = "jump"` were deleted. The frameworks are
ordinary modules now: ColdFire assembly (`modules/core/adapters/machines/src/cf/dt2/machines.s`, `.../dn2/machines.s`,
`modules/core/adapters/params/src/cf/dt2/params.s`), a manifest with registries (`machines`, `params`, `voices`), P1 copies of the
stock tables (`[[section]]` with one appended row per entry), bound and reference writes (P3, P6) and hooks (P4). The sites,
the behaviour and the evidence above stand; what changed is who builds the tables and how a site is reached.

- **Tables at run time vs patch time.** Name table, permission masks, per-type bytes and the DSP selector are P1 copies of the
  image's tables with the clone's row appended (`image.u32(table + 4 * $clone_of)`; nothing of the firmware is stored). The
  displayed list (stock order with every machine inserted at its `position`), the rank pairs of the sort comparator and the
  std::string descriptors (upper-cased names) are built once in bss by `mf_ensure`, the first time a routine needs them.
- **Clone tests** (`type == MANUAL SLICE`, 6 sites) are hooks that replace the whole stock test (prefix, compare, branch):
  the routine takes the stock branch itself and answers "equal" also for an added clone of SLICE. This needs the branch
  target of each test as an anchor (`cf.machines.clone.N.alt`). A first version hooked the compare and set the Z flag: wrong,
  the displaced compare ran after the routine and undid it (XSLICE sounded like SAMPLE); the unit harness could not see it
  because it stubs the tails. The proofs did.
- **Type argument** of the parameter mapping (slots 29, 30) and the Digitone II type maps change the function's own stack
  argument in place instead of a register after the load (a hook runs before the displaced load).
- **Group helper, Digitakt II [D]:** the site `moveq #6,D1; cmp.l D0,D1; bne` is only reached for a type above 5 (the
  function answers 2 for type 5 and 1 for 0..4 before it), so `bcs` with the raised bound changes the answer for added types
  only; no stock type changes.
- **Unit tests:** `tools/patchkit/pk_cfunit.py` runs one hook routine on the native core with chosen registers, flags and stack (tails and
  exits are stubs that report where the routine left); `tests/patchkit/test_pk_machines_fw.py`, `test_pk_machines_dn2_fw.py`,
  `test_pk_params_fw.py`; the manifest row expressions are evaluated by `tools/patchkit/pk_expr.py` and compared with the old Rust
  row builders' formulas. Integration: `packages/patchkit/tests/frameworks.rs` (real modules on the real images).
- **Hot-path cost matters to the proofs [D]:** the Digitone II hooks on the frame builder (vector 191), the type reader and the
  type maps run for every audio block or parameter access. A first version saved registers before it knew the type was a
  stock one; the patched image then cold-booted half a DSP block (32 frames) off the stock timeline and the exact
  PCM comparison of `pk_proof_voice_dn2.py` part a failed by -55 dB (envelope block quantisation, not content). The routines
  now test `type < machines.first` first and touch no register on that path; the comparison is exact again (shift -64).

## DN2 own parameter pages: built (patchkit, lane b) **[D]**

Emulator only, one agent, not checked by a second one: **[D]**. Realises "DN2 parameter pages" above as modules; the research
there stands except where marked **[C]**. Code: `modules/core/adapters/machines` (DN2 assembly `src/cf/dn2/machines.s`, registry `pages`),
`modules/core/adapters/params` (the Digitone II side: registry, `pf_table` section, hooks and writes for 321 rows, rules in
`modules/core/adapters/params/module.toml`), `tests/fixtures/modules/dn2-pages` (the content proof), `tools/patchkit/pk_proof_pages_dn2.py` (quick and full),
`tests/patchkit/test_pk_machines_dn2_fw.py` (the routines on the ColdFire call harness, 5 new tests). No change in `packages/patchkit`.

**What a content module writes.** Parameters as on the Digitakt (`registry = "params"`: `name`, `short`, `template` = a stock code
of the same value class, `slot` = the **sound-array slot itself** 25..64, `min`, `max`, `default`) and one row per page:

    [[entry]]
    registry = "pages"
    id = "pg1"
    machine = "machines.pages.index"    # the type; pages are numbered in entry order
    name = "PGA"; title = "Pages Test"  # the std::string pair of the page descriptor (the title is the screen header)
    c0 = "params.pg_alpha.index"        # knob A .. H: a parameter code (own or stock), 0 = no knob; c8 = 10 (track level)
    s0 = 25                             # the sound-array slot of that code's row

`core/params` on the Digitone II appends T1 rows (60 bytes: the template's owner, flag, CC word, key and formatter; our slot, range,
default and names) to a copy of the 321-row table, repoints the 56 references and raises the 55 bounds (the Digitakt rules
resolve unchanged, `cf.params.ref.N`, `cf.params.bound.N`, `cf.params.t1/t2`), and the T2 accessor shim maps an added code to its
template's value-class object (A Env Decay, `0xd5`, plain 0..0x7f00 words, for all of the proof's parameters).

**What changed in the hooks, against the research.**

| research said | built |
|---|---|
| a 40-word slot-table row per type | **no table**: `s0..s8` of the page rows are the slots of the page's knobs; `mf_pg_slot` scans the type's rows for the slot (about 9 compares per page), so authors write each (code, slot) once per page; the hook `typemap.2` answers the code for a machine slot 25..64 (0 for an unmapped slot) and otherwise runs the stock code (it ignores the type there) |
| a `{count, begin}` page record per type | no record: `mf_pg_count` / `mf_pg_page` scan the registry; `typemap.0` answers the count, `typemap.1` returns **the registry row as the page descriptor** (it is laid out as one: two std::string character pointers, nine codes, then the machine and slots after it); a page outside the count falls to the stock code with the own type (> 4), which answers MIDI's row |
| std::string pairs constant | built once at run time by `mf_ensure` (as the machine names): `{length, capacity, refcount -1, chars}` reps in bss, the row's two pointers rewritten in place |
| ~17 reader call sites get the own type (`reader_call.9..`) | **[C] none**: a machine with page rows is its own type for **every** caller of the reader (one test in `mf_reader`: the type has pages). The list was incomplete: the defaults on selection (`SoundParamChangedInfo::ctor_dtor` at `0x4004c330` / `0x4004c48c`, which call the reader and then `FUN_400dc18e` per slot) also need the own type (with the clone's type the sound got FM TONE's defaults: first version), and so do the clipboard functions that feed `FUN_40045c82` / `FUN_4004610a`. Machines without page rows are unchanged (clones) |
| T1 owner field | the template's owner (0 = FM TONE); the page layer never tests it for an added machine (the owner maps are built from stock rows only; **[O]** MIDI CC and NRPN and the owner-keyed lookups ignore added rows as on the Digitakt) |

Register contracts found on the way **[D]**: the stock `FUN_400dc18e` sets `D1 = 0x64` **before** the shim site and reads it again, so
`typemap.2` restores it (an early build clobbered D1; the unit test now pins it); `typemap.0/1` may use D0, D1, A0 (the stock code
reloads or sets them after the site); A0 holds the slot at `typemap.2`. The reader (`FUN_4004b7f2`) calls virtuals itself, so D1,
A0 and A1 are free there.

**Evidence** (`tools/patchkit/pk_proof_pages_dn2.py`, `test/dn2-pages`: type 5 `PAGES`, page 1 `ALPH BETA GAMA DELT` on slots 25, 26, 27, 30
with ranges 0..0x7f00, 0..0x3f00, 0x100..0x7f00, 0..0x7f00 and defaults 0x2000, 0x1000, 0x7000, 0x4000, page 2 `EPSL ZETA` on 40, 41
with defaults 0x800, 0x7f00; every input asserts no fault, halt or stall):

| check | result |
|---|---|
| the machine list, YES | PAGES is the 5th entry (before MIDI); the reopened menu marks it; the audio mirror holds type 5 |
| pages on screen | titled `Pages Test (1/2)` and `(2/2)`, knob labels `ALPH BETA GAMA DELT` and `EPSL ZETA`, two pages in the SYN cycle (frames 1, 2, 1) |
| page rows read back from the running firmware | names `PGA`, `PGB`, title `Pages Test`, refcount `0xffffffff`, codes `0x141..0x144` (page 1), `0xa` last, machine 5 |
| knobs (30 events of +4 detents 2 ms apart, TRIG 1 held) | A, B, C, D of page 1 move exactly the frame words of slots 25, 26, 27, 30 of the held voice (`0xda + 0x92 v + 2 (s - 25)`); E..H nothing; page 2 A and B move 40 and 41 (B turned down: its default is the maximum); empty knobs nothing |
| ranges | BETA stops at `0x3f00`, GAMA at `0x100` (minimum) and `0x7f00` |
| defaults after selection and a note | `0x2000 0x1000 0x7000 0x4000` and `0x800 0x7f00`: the machine's, not FM TONE's |
| save, set FM TONE, load | PAGES is the track's machine again, the values are back (the words after the smoother settled); the card's sound records hold type 5; **stock** loads the same project (no fault, track 1 is FM TONE) |
| stock machines | FM TONE SYN (4 pages), FLTR and AMP frames of the patched image equal the stock firmware's |
| `test/dn2-clone` (a clone without page rows) | still proven by the `core/services/machines:bind` target and the machine fixture suite (`core/runtime-fixture-tests`) |

**Open [O].** MIDI CC / NRPN and p-lock for an own-pages machine (not tried); copy and paste of a sound between an own-pages machine
and others (the clipboard callers get the own type; not run); the FLTR, AMP, FX and MOD pages of an own-pages machine were not compared
frame by frame (they showed no seam in the PLUCK proof's runs); the widgets are the generic eight-knob page only (no algorithm or
envelope views); hardware never flashed. Estimated the work at 1 to 2 days; it took one day of agent time.

### PLUCK on Digitone II (`tools/patchkit/pk_proof_pluck_dn2.py`, 1.12) **[D]**

`digikit/pluck` builds for the Digitone II (device-filtered entries and object): type 5 (`own_dsp`, so the FM engine renders nothing and the
voice hook does; listed before MIDI), parameters `pluck_decay`, `pluck_pick`, `pluck_bright`, `pluck_color` (template `0xd5`, slots 25..28,
range 0..0x7f00, defaults 0x4000, 0x2000, 0x5000, 0x2800), one page `PLUCK` / `Pluck` with those four on knobs A..D and the track level last.
The routine and its contract: finding 06, "DN2 voice routine: PLUCK". Evidence (quick and full proof; the numbers are 1.12):

| check | result |
|---|---|
| select, defaults | the audio mirror holds type 5; the held voice's words 25..28 are `0x4000 0x2000 0x5000 0x2800` |
| tuning, TRIG 1 and keyboard keys (the pitch word of the voice slot against the fundamental of the rendered note) | notes 60, 61, 63, 66, 67, 72: +0.0037, -0.0045, +0.0001, +0.0019, -0.0016, -0.0002 cents |
| DECAY (BRIGHT at the top, key held so the engine's gate stays open) | T60 0.23 s at 0x2000, 0.96 s at 0x4000 (formula 0.98 s), 2.16 s at 0x5000 (formula 2.16 s) |
| BRIGHT | HF/LF 0.2 s into the note -13.3 dB at 0x800, +11.8 dB at 0x7f00 (+25.0 dB) |
| COLOR | excitation HF/LF 16.7 dB at 0, -5.9 dB at the top (22.6 dB lower) |
| PICK, COLOR, amplitude, N | the excitation the DSP writes into the slot's delay line (the last N + 1 samples, read from DSP memory 80 ms after the note-on) equals the numpy model `tools/patchkit/pluck_ref.py` seeded with the DSP's noise state: **bit for bit** (max abs difference 0) at PICK 0, 0x2000, 0x3f80, 0x7f00 and COLOR 0, 0x7f00 (comb p = 0, 23, 44..46, 91). The audio statistic of the Digitakt proof (harmonics 4 and 8 down against 2, 3, 5, 6, 7, 9 at PICK = 1/2) is a weak test: the noise excitation scatters one pluck's harmonics by several dB (the model itself gives 8.5 dB over six plucks against -0.7 dB without the comb, the engine's filter adds a tilt), so the proof prints it and requires only +2.5 dB over six plucks |
| the edge, repeated notes | 20 notes of TRIG 1 150 ms apart: 20 plucks of note 60 (sampled in tune); 40 notes 110 ms apart (past 16 voice slots, 16 strings overlapping): 40 plucks; a note after the burst sounds |
| sequencer | a step recorded on track 1 plucks on both passes of the pattern (onsets at 2.0 s and 4.0 s, the first at note 60: +0.24 cents) |
| save and load | DECAY and BRIGHT edited, project saved, track set to FM TONE, loaded: PLUCK (row 4) and all four values back; the card's sound records hold type 5; stock loads the project (track 1 is FM TONE), no fault |
| harness | `tests/patchkit_smoke` cell `dn2-1.12: core/machines, core/params, core/dsp-voice, digikit/pluck` (select, own page, trig sounds, BRIGHT knob moves the page) |

**Differences from the Digitakt PLUCK [D].** No TUNE and no LEV rows (the engine's level is the last knob; pitch is the voice's pitch word);
the note's end is the engine's amplitude envelope and gate, so a long DECAY is heard only while the key is held or the AMP release is long
(the T60 measures hold the key); the amplitude of a note is the velocity word like the Digitakt's.

## Stable machine type numbers (patchkit, lane b, 2026-10-14) **[D]**

**Problem [C].** `core/machines` numbered the added machines `machines.first + k` in canonical module order. The number is
all a project stores ("Persistence of machine types", point 2), so the same project loaded as a different machine, or as
type 0, as soon as another machine module was added or removed. The old rule is replaced.

**Rule.** A machine's type is its row in `modules/core/adapters/machines/types.toml`: per device, `"<module id>:<entry id>" = number`,
and `last`, the highest number the framework supports. Numbers are committed with the module, never reused or changed; a
removed machine keeps its number retired. patchkit (generic, no machine code): the registry key `index_table` names the file,
`[registry.gap]` gives the field values of a filler row. It refuses an entry without a number (the message names the key to
add), two entries with one number (checked when the table loads), a number below `first` (stock types are never allocated)
and a number above `last`. `mise run patch:new -- machines/NAME --machine` appends the next free number per device and says so.

| device | stock types | first number | `last` | why |
|---|---|---|---|---|
| Digitakt II 1.16 / 1.17 | 0..6 | 7 | 23 | the DSP type -> selector table was read to entry 23 (finding 06), the entries above are not known; the frame word of type t is `0x94 + 2t` |
| Digitone II 1.11 / 1.12 | 0..4 | 5 | 23 | chosen, equal to the Digitakt's. Firmware bounds that were read: the persistence byte (`(u8)(type + 1) < imm`, so up to 254), page tables built per type by core/machines, the FM engine knows 0..4 and treats the rest as MIDI. No table that caps the type lower was found **[O]**: the DN2 limit is a choice, not a measurement |

Allocated: Digitakt II `test/xslice:xslice` 7, `test/machine-clone:xclone` 8, `digikit/pluck:pluck` 9; Digitone II
`test/dn2-clone:fmclone` 5, `test/dn2-pages:pages` 6, `test/dn2-sine:dn2sine` 7, `test/dn2-edge:dn2edge` 8, `digikit/pluck:pluck` 9.
(`test/sine-voice` names type 8 as a literal on purpose: it adds no machine, the proof forces the wire type; it claims
`machine.type.8`.)

**Gaps.** The tables of the framework are indexed by type, so the registry spans `first ..= the highest selected number`
(`machines.count` = rows including gaps, `machines.entries` = real machines). A number nobody selected is a **gap row**: a
clone of type 0 (permission, per-type byte, DSP selector word), empty names, `position = 0xffffffff`. The run-time list
builder skips a row with that position, the rank pairs and the list length use `machines.entries`, so the machine list shows
only the selected machines; the bounds (name, permission, per-type, group, persistence immediate) use the span, so a stored
type that names a gap loads and plays as type 0 (SAMPLE or FM TONE). PLUCK alone on the Digitakt is type 9 with rows 7 and 8
as gaps; the plan lists them (`gaps`). The sizes of the stock tables copied into the expansion region and `dsp.l2` grow by
one row per gap (4 to 64 bytes each).

**Evidence.** `cargo test -p patchkit` (prims: numbers follow the table, not the selection order, a subset keeps its numbers,
gap rows take `[registry.gap]`, the five refusals; frameworks: PLUCK alone is 9 with gaps `[7, 8]`, the descriptor row holds
the module's parameter codes, row 7 is a clone of type 0 and hidden); `tests/patchkit/test_pk_types.py` (every machine entry has a number
on each device it targets, numbers unique and in range, `pk_new.allocate`); the ColdFire call harness tests of the list
builder (`tests/patchkit/test_pk_machines_fw.py`, `_dn2_fw.py`) pass unchanged. Emulator: see the results note in
docs/plans/patchkit.md (quick-tier proofs and the smoke matrix on the permanent numbers).
## Digitone II 1.11: machine selection in the emulator, and where a track becomes MIDI **[D]**

Measured 2026-09-18 on Digitone II OS 1.11, MAIN OS loaded at `0x40000400`, in the
Python reference emulator of that time, which is removed since. That run also had three
changes that were never merged: #19 (DN2 boot fixes), #24 (resume weakptr snapshots)
and #28 (`--patch-ranges`, `--poke-at`, `--stack-when`). The addresses are the
firmware's and do not depend on the emulator.

### Getting there **[D]**

- `boot400M` is still booting: FUNC+SYN opens the selector at ~70M, and the
  +Drive loading screens then take the panel over. A run from `boot400M` to
  +800M reaches the SYN page (LIGHTHOUSE loaded by +200M); a snapshot saved
  there (`ui1200M.snap`) is where menu work starts.
- Resuming a snapshot that `guirun --save-at` wrote needs the timers claimed
  (`deferred_components=('timers',)`, which `main` does); an older `guirun`
  fails with `checkpoint missing configured components: timers`.
- DN2 key codes follow the bootstrap's KEY TEST table, which uses the shared
  platform names: **FUNC 17 + SRC 2** is the DN2's FUNC+SYN, then UP 11,
  DOWN 14, LEFT 13, RIGHT 15, YES 10, NO 12.

### The selector **[D]**

MACHINE SEL has two pages; LEFT/RIGHT switch them (the owner's tip, confirmed).
The menu's own lists (`view+388` SYN, `view+400` FLTR, 8-byte entries; cursor
`view+420`, page `view+424`, committed type `view+412`) give each row's type:

| SYN row | type | FLTR row | type |
|---|---|---|---|
| FM TONE | 0 | MULTI-MODE | 0 |
| FM DRUM | 2 | LOWPASS 4 | 1 |
| WAVETONE | 1 | LEGACY LP/HP | 4 |
| SWARMER | 3 | COMB- | 3 |
| (divider) | -1 | COMB+ | 5 |
| MIDI | 4 | EQUALIZER | 2 |

Type 4 = MIDI agrees with DNX's reading of saved kits.

### The commit **[D]**

YES calls `0x40031880(model, track, type)` from `0x4005bba0`; the type comes
from `0x4005b162(view)` (row -> list entry -> `0x40116e54`). `0x40031880`:

- sets the kit's per-track MIDI mask at `kit+0x5cda` -- **bit `track` set exactly
  when `type == 4`**, cleared otherwise (`0x400318d0`) -- written at `0x40031902`;
- calls `0x4004cc08(sound, type, 0)`, which writes the machine type at
  **`sound+0xde`** (`0x4004cc94`; the sound for track `t` is `kit + 52 + 1163*t`);
- the frame ISR's mirror of the mask at `0x8000537c` follows in the same
  instant (`0x40025b9a`).

All three writes were caught with `--watch` at one instruction count.

### An input artefact worth knowing **[D]**

A `--input` press is held for ~9M instructions before its release is serviced,
which is long enough for the firmware's key repeat: one DOWN from WAVETONE left
the model's cursor on **row 5 (MIDI)**, while the panel still drew row 3
(SWARMER) highlighted. YES then committed MIDI. Read the cursor (`view+420`)
rather than trusting the panel after a held key; a shorter hold would need the
press and release serviced closer together.

## Knob slots 3 and 6 on a run-time SAMPLE clone (DT2 1.17)

**[D]** A run-time machine that clones SAMPLE (test/dyn-pluck, type 18) was packed with an extra parameter on slot 6
(mirror 33), then on slot 3 (mirror 28), range 0..0x7f00, and driven in the emulator (page encoder, Sound mirror, TX
frame word, note PCM; 2026-10-20). Slot 6: the TX word equals the stored value at every step (0..0x7f00) and the note
PCM equals the control with the slot empty: usable. Slot 3: the stored value follows the encoder, but the TX word is
`value & 0x3ff` (0x1234 -> 0x234, 0x7f00 -> 0x300): the DSP sees only the sample number bits, so slot 3 is reserved
on a SAMPLE clone (docs/design/module-sdk.md 11.5 D1).

**[D]** Slot 3 on a track with a sample assigned (2026-10-20, patched-in digikit/pluck with `pluck_decay` moved to knob 3, SRC page
encoder 4, Sound mirror 28 and the TX frame word after a trig, one bounded run). Control, stock SAMPLE: mirror 28 is the sample
number (7 at the start, 0xf and 0x16 after two bursts), TX word equal to it. Clone with the module parameter on knob 3: the module
default 0x4000 replaces the sample number at selection (the DSP sees 0), the stored value follows the encoder (0x3c00, 0x6e00,
0x7a00) and the TX word is `value & 0x3ff` (0x0, 0x200, 0x200). The DSP therefore sees only the sample number bits of a value that
moves in raw steps, and the track's sample assignment is lost: slot 3 is not usable with a sample assigned either.
