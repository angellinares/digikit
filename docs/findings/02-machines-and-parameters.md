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
`tools/machinepatch.py` runs both arms. **[V]**

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
`tools/machinepatch.py --milestone b`.

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
after it. Disassembling the emitted bytes back with `dt2.coldfire.disasm` and
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
in `tools/machinepatch.py` and selectable part by part with `--patch-machine`:

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

`tools/machinepatch.py` now takes a `MachineSpec` (display names, descriptor
names, the stock type to copy fields from, list position, optional fields),
and `plan_b` computes every write from a `read(addr, n)` function. With the
default spec it produces the same 18 writes as the verified run, live and
against the static image (`tests/test_machinepatch_plan.py`). With
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
tests the type number. Three more parts in `tools/machinepatch.py`, driven
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
mapped images are in `tools/machineprofile.py`.

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
the IDs as immediates. `tools/machinedescr.py` parses that initializer (with a
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
field at `+4`. `tools/paramtable.py` dumps and decodes it. Entry layout:

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

The joined table (`tools/machinedescr.py` + `tools/paramtable.py`):

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
either entry's `0x3c` span, and `tools/refscan.py` over the whole image (96.78%
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
stub in the confirmed-zero cave tail (`tools/machineprofile.py` `cave_b =
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

`tools/machinebuild.py --profile dt2-1.16 --machine XSLICE:XSL:6:7 --fields
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
