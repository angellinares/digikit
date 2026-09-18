# Handover 2026-09-16: the machine type reaches the SHARC, and the language

Replaces `HANDOVER-2026-09-15-sharc-side.md`. Results are in
`docs/FINDINGS.md`; this file holds state and next steps only.

Two strands run in parallel now. The machine work is the one with a clear next
move, and "Start here" below gives the order. The SHARC+ language work under
"Next steps, in order" is the road both strands eventually need.

## State

- **Everything is merged to `main` and pushed.** The `sharc-pcode` branch
  landed as PR #17, and six more PRs landed on 2026-09-16 after it: #4, #16,
  #15, #12 and #3 from the external contributor `angellinares`, then #18, our
  follow-up corrections. Nothing is uncommitted except the untracked files
  listed below.
- **`main` is protected: a direct push is rejected, and changes must go through
  a pull request.** `gh pr merge <N> --merge` works and needs no approving
  review. Commit on a branch from the start rather than on `main`.
- Tests: 209 passed, 5 skipped, 24 subtests passed.
- **`emu/symbols.py`'s `mainloop` signature was broken on both analysis
  targets** until PR #16 landed on 2026-09-16. It resolved to zero matches on
  Digitakt II 1.16 and Digitone II 1.11 alike, because byte +15 of its
  signature is a `moveq` immediate that changed, and that silently made
  `tools/bootcheck.py` report `PARTIAL_MAIN_OS` / `MISSING: mainloop entered`
  on runs where the OS was in fact running. **Any bootcheck verdict recorded
  for 1.16 or 1.11 before 2026-09-16 is suspect and should be re-run.** It now
  resolves to `0x40033d00` and `0x4002f178`.
- **PR #11 is open and waiting on its author, not on us.** It is a SHARC+ VISA
  encoding cross-check whose load-bearing claim the author retracted in his own
  comments. He was asked to revise it down to what survived, and for the 31
  classes and 577 codes he extracted from the Core Programming Reference's
  chapter 27, which would serve stage 2 below. `angellinares` is an active
  external contributor who self-corrects unprompted and twice found real bugs
  independently: read his PRs properly rather than merging or closing on the
  title.
- The language installed in Ghidra is current: 80 constructors, slaspec
  `ca2362aa`, installed by `tools/ghidra/install-sharc.sh` on 2026-09-16. The
  programs in `~/ghidra-projects/elektron-sharc` were imported under two older
  languages now: re-import before reading them, or work from the throwaway
  projects `tools/sharcpcode.py` builds under its own output directory.
- Measurement runs, git-ignored, each with `lint.json`, `<image>.json`,
  `<image>.sqlite` and its own Ghidra project, under `out/sharcpcode/`:
  `db-old`, `db-new`, `t21`, `t22`, `t23`, `t24`, `t25`, `t26`, `t27`, `t28`,
  `t29`, `t30`. **Measure a change against `t30`.** `t24` to `t27` compiled a
  stale slaspec: their decoder numbers are sound but their Ghidra numbers are
  of the language as it was before `78a4ee5` -- see the Gotchas. `t28` is lint
  only (the guard stopped it); `t29` and `t30` are the first runs whose
  language matches the decode table.
- Analysis targets are Digitakt II 1.16 and Digitone II 1.11; Em asked on
  2026-09-15 to stop cross-checking 1.15C.
- The device stays on 1.15C with bootstrap 2.00. Installing 1.16 upgrades the
  bootstrap to 2.01 and cannot be undone, so no hardware test of a
  1.16-based build happens without Em's decision.
- Done: steps 1-4 of the retarget (1.16 in Ghidra with names carried from
  1.15C, the frame link re-found, a 1.16 cold-boot ladder, the gate writer, a
  frame captured on 1.16), and on the SHARC side the decoder rebuild, the
  generated Ghidra language, both DSP images imported, and the
  `docs/sharc/structure-1.16.md` hypotheses checked (call convention, RPC
  dispatcher task, command block confirmed on 1.15C and 1.16).
- `sections/` and `snapshots/` are 1.15C, and Em uses them. Other outputs,
  all git-ignored:
  - `out/sections/dt2-1.16/`, `out/sections/dn2-1.11/`,
    `out/sections/dn2-1.10E/`: extracted sections.
  - `out/snapshots/dt2-1.16/boot{60,120,200,280,400}M.snap` and
    `.ladder.json`: the 1.16 cold-boot ladder.
  - `out/ghidra/{dt2-1.15C,dt2-1.16,dn2-1.10E,dn2-1.11}-emac/`: ColdFire
    dumps made before Version Tracking.
  - `out/vt/`: Version Tracking exports and `check-*.json`.
  - `out/maps/`: dspmap and gatewatch outputs. `out/sharcframe/`: frames.
  - `out/sharc/dt2-{1.15C,1.16}-main.bin`: the DSP main programs (104,848
    bytes at loader byte address `0x28382670`); `compare-*.json`.
    `out/sharc/dn2-1.11-main.bin`: Digitone II 1.11 (105,016 bytes, SW
    `0x1c12e2`).
- Ghidra, outside the repo:
  - `~/ghidra-projects/elektron-emac`: ColdFire programs `/dt2-1.15C`,
    `/dt2-1.16`, `/dn2-1.10E`, `/dn2-1.11`, `/dn2-1.11-from-dt2`
    (`section_3_MAIN_OS.bin` in each), Version Tracking sessions in `/vt/`.
  - `~/ghidra-projects/elektron-sharc` (language
    `SHARC_VISA:LE:32:default`): `/dt2-1.16_SHARC` and `/dn2-1.11_SHARC`
    re-imported with the CJUMP language and after `tools/sharcflow.py
    --cover` (1,999 and 1,776 functions);
    `/dt2-1.15C_SHARC` (352 functions, not covered); test copies in
    `/flowtest/`, `/flowtest2/`, `/flowtest3/` and `/flowtest5/`, which can
    be deleted.
  - `~/ghidra-projects/backup-2026-09-15-sharc`: `/dt2-1.16_SHARC`,
    `/dt2-1.15C_SHARC` and `/dn2-1.11_SHARC` from before the flow pass.
  - `~/ghidra-projects/dt2-emac` (1.15C ColdFire, unchanged content),
    `backup-2026-09-15-elektron` (`/dt2-1.16` before Version Tracking),
    `backup-2026-09-14`, `dt2cmp` (old Digitone II 1.10E import), `dt2`
    (old, holds `dt2_SHARC` imported with the old language).
  - Installed processor modules: `Processors/SHARC_VISA` (current, from
    `tools/ghidra/install-sharc.sh`), `Processors/SHARC` (old, keep for
    `dt2_SHARC`); ColdfireEMAC is linked into the user Extensions.
- `tools/framelink.py` holds the ColdFire frame-link addresses of 1.15C and
  1.16 by MAIN OS SHA-256.
- `tools/sharcimm.py` outputs in `out/sharc/`:
  `imm-periph-dt2-{1.16,1.15C}.json` (code immediates) and
  `words-periph-dt2-{1.16,1.15C}.json` (loader-block words).
- Untracked and not for committing: `maybe.md`, `scratch/`,
  `docs/refs/dspi2-edma-blocker-and-register-sources.md`. Stage by explicit
  path only, never `git add -A`. (`docs/refs/netburner-coldfire/` is listed in
  earlier handovers but no longer exists.)
- New and tracked on `main` since the merges:
  - `tools/machineprofile.py`: the machine-type anchors of three images.
  - `docs/SHARC-ADDRESS-MAP.md`: the exec-to-load map and which regions of the
    Digitone II blob are code. Its L1 and L2 labels were the wrong way round
    when merged and were corrected in #18; `0x28xxxxxx` is L1 and
    `0x20000000` is L2.
  - `docs/refs/dn2-dspi2-cross-check-2026-09-16.md`: an independent read of the
    DSPI2 frame on Digitone II 1.11.

## Start here: the machine type reaches the SHARC -- find its reader

Answered on 2026-09-16. The fork this section used to pose is closed, and the
answer is the one that keeps stages 1-7 below on the critical path for
Digitakt II as well as Digitone II.

A track's machine type is sent to the DSP verbatim, per track, in every frame:
the big-endian word at TX offset `0x94 + 2i` (FINDINGS, "The machine type
reaches the SHARC, at TX frame offset `0x94 + 2i`"). Two independent methods
agree. Statically, the handler loads the SRAM type byte into D7 at
`0x4002eb4a` and stores it at `0x4002ebe0` (`move.w D7w,(0x94,A3)`). In the
emulator, poking that SRAM byte moves frame byte `0x95` to 4, 5 and 6 in step,
across three values and two passes.

Two reasons earlier passes missed it, both worth remembering:

- The handler never reads the machine-type field of a track object. It reads a
  **copy** of that byte in SRAM, put there by `FUN_4002d438`. No xref query and
  no `tools/refscan.py` sweep for `+0xa2` could have found it.
- A first reading of the handler followed D7 to its `tst.b` at `0x4002eb68`,
  concluded the type was reduced to a boolean, and stopped one instruction
  before the store. The `tst.b` is a second, independent use of the register.

So a new machine is not a ColdFire-side concern only, and "a machine is just a
parameter mapping" is not available as the easy answer on Digitakt II.

**The next question, and it is now a sharp one: what does the SHARC do with
the value at receive offset `0x94 + 2i`?** The DSP receives 2050 bytes. Find
the code that reads offset `0x94 + 2i` of that buffer and see whether it
branches on the value or merely stores it. If it only stores it, a new machine
may still need no DSP code; if it indexes a table of routines with it, it does.

That read needs compute semantics in the generated language, so it sits behind
stage 4 below, and step 1 (the SPI2 trace, "Where the SHARC receives the
ColdFire's frame") is the way in -- specifically its last bullet, following
the receive buffer to its reader.

Three things this work also produced:

- **A per-track TX frame map** in FINDINGS: which SRAM byte lands at which
  frame offset, for eleven per-track fields. This is the ColdFire-to-SHARC
  interface, and it is what to match the DSP's receive path against.
- **Corrections to the SRAM layout**, also in FINDINGS. The `0x9a`-stride
  per-track table is at `0x80003cd0`, not `0x80003340`: the handler addresses
  a row through a base register at `0x80003340 + i*0x9a` plus a `+0x990`
  displacement. `tools/framelink.py`'s `TABLES` entry
  `(0x80003340, 0x9a, 16, 'track_9a')` therefore names the wrong 2,464 bytes.
  **Not yet fixed in the tool.**
- **`tools/machineprofile.py`**, new: the machine-type anchors of Digitakt II
  1.15C and 1.16 and Digitone II 1.11, keyed by MAIN OS SHA-256, with byte
  preconditions at fixed addresses. 18 of 18 byte checks pass on the two
  Digitakt images; the Digitone II profile carries no checks yet and reports
  itself as unchecked rather than as passing. Run it with
  `uv run python tools/machineprofile.py IMAGE --names`.

Still open from the experiment: the type == 4 or 6 branch that sends slice
boundaries to the frame was never reached, because it is gated on a slice
count in the machine-set message and a stock boot has no sliced sample loaded.
`0x800033a0` was never written in any run, patched or not. Testing that path
needs a track carrying a sample with slices.

### The order to work in

Agreed with Em on 2026-09-16. Steps 1a and 2 are unblocked and static; start
there.

| | | blocked by |
|---|---|---|
| 0 | Port the emulator literals to 1.16, so a UI-driven run works at all | -- |
| 1a | Static: does the machine-commit path actually reach `FUN_4002d438`? | -- |
| 1b | Headless: hook it, then call it with a changed type | 1a |
| 2 | The SHARC's reader of receive offset `0x94 + 2i` | -- |
| 3 | Decide on type substitution | 1 |
| 4 | Port `tools/machinepatch.py` onto `tools/machineprofile.py` | -- |
| 5 | Patched image, then hardware | 4, and the bootstrap decision |

**0** is the real blocker for anything that needs a machine to be *selected*
rather than simulated. Section 5 below lists the 1.15C literals still in
`emu/longrun.py`, `emu/edma.py`, `emu/panel.py`, `emu/screen.py`, `emu/hle.py`,
`emu/serial.py`, `emu/uiprobe.py` and `emu/gui.py`, and
`out/vt/check-dt2-1.15C_to_dt2-1.16.json` gives the 1.16 counterpart for most
of them. It does not gate 1a, 1b or 2, so run it alongside rather than in
front.

**1a** matters because the chain has a verified back half and an assumed front
half. The handler forwarding the byte is proven. `FUN_4002d438` populating the
SRAM row *from a machine commit* is not. Its known callers on 1.16 are
`FUN_4002d9c4` and the handler itself, and `FUN_4002d9c4`'s own callers look
like pattern and track **load** sites, so the row may only refresh on pattern
load rather than on a commit. Answerable from the 1.16 dump with no emulator.

**1b** needs no GUI. Hook `FUN_4002d438` on a plain resume from
`out/snapshots/dt2-1.16/boot400M.snap` to see whether it fires at all and with
what arguments, which yields a real source-object pointer; then use
`emu/harness.py`'s `call(machine, func, args)` to invoke it with a changed type
byte and watch both the SRAM row and the captured frame.

**3, type substitution**, is the design that falls out of the finding: keep the
new type number for the ColdFire UI and its parameters, but write a stock type
into the SRAM byte the frame reads, so the DSP and the slice path see a
known-good engine. That makes a new Digitakt machine sonically identical to its
clone source by construction rather than by hope, and it removes the question
of what the DSP does with an unknown type. Decide it only after 1 says what is
being sent today: the `permit` part of `tools/machinepatch.py` raises the bound
in `FUN_400da3b0`, which is the same function that gates the SRAM copy, so the
existing PLACEHOLDER build may already be sending type 7 to the DSP.

**5** collides with the 1.16-only rule, because hardware means installing 1.16
and upgrading the bootstrap irreversibly. That is Em's decision, not a task.
Note that the MAIN OS build floor is **not** a second obstacle: Digitakt II
1.15C's BUILD string is `0071` against a `"006/"` floor, so it clears
(FINDINGS, "MAIN OS has a second gate, and it reads the BUILD string").

## The machine template, for both devices

Em's goal is their own machines on Digitakt II and Digitone II.
`tools/machinepatch.py` installs an eighth machine and works, but every
address in it is a 1.15C address, so it runs on exactly one image.
`tools/machineprofile.py` now holds the same anchors for three images. The
remaining work, in order:

1. Make `tools/machinepatch.py` take a profile instead of its module-level
   constants. `plan_b(read, cave_b, parts, eighth, spec)` is already pure --
   every byte comes from `read(addr, n)` or the spec -- so it can be driven
   against a static image as well as live guest memory. Thread a profile
   through it and delete the constants. `tests/test_machinepatch_plan.py`
   pins the 18 writes the default spec produces on 1.15C, so the refactor has
   a fixture: it must produce the same 18 writes.
2. Generalise the count. The tool assumes seven stock machines and exactly one
   new type numbered 7 in a dozen places: `ORIGINAL_TABLE`, `NAME_TABLE_ROWS`,
   the `read(PERMIT_TABLE_SRC, 28)` and `read(PERTYPE_TABLE_SRC, 7)` widths,
   `validate_spec`'s `range(7)`/`range(8)`, `MachineSpec`'s defaults of 6 and
   7, and `run_b`'s `expected` dict. Digitone II has five. Take the count from
   the profile.
3. Digitone II's differences that break a shared template, from FINDINGS
   ("Digitone II 1.11 has the same machine machinery, with five machines"):
   the type byte is at `+0xde` not `+0xa2`; the name-table rows are
   `{hint, name, abbrev}` not `{name, abbrev, hint}`; there is no list filter
   table, no sort comparator (so the `rank` part -- the boot blocker on
   Digitakt -- may have nothing to patch), and no per-type byte table. All
   three parts must become optional rather than assumed.
4. Digitone II's real cost is not the descriptor. Its machines are synthesis
   engines with their own parameter pages (`MachineParameterPageView`,
   `SrcMachineParamPageCopy`, `FilterMachineParamPageCopy`), which Digitakt
   has no counterpart for. A sixth machine there needs a parameter page, not
   just a descriptor and a name row.
5. Only then, a patched image rather than patched guest memory: `plan_b`
   against a static `section_3_MAIN_OS.bin`, `tools/patchimg.py` to apply it,
   `tools/roundtrip.py` to prove the container rebuilds. That is the
   "boot from a patched image" item under "Still open from 2026-09-14".
   Note that `clone_of == 6` is the only case `plan_b` can compute statically:
   the descriptor array is bss, so any other clone source needs `spec.fields`
   passed in explicitly.

One gotcha found the hard way while mapping these anchors: **both images load
at `0x40000400`, not `0x40000000`.** An agent using the wrong base read a
table 1,024 bytes downstream, reported it as consecutive small integers, and
built a confident false finding on it. The same error looked like a capstone
desync in a second place. Check the base before believing a raw byte read.

## How to run

```
uv run --with pytest python -m pytest tests -q

# SHARC: language, import, main program, decoders
tools/ghidra/install-sharc.sh
uv run python tools/sharc_import.py out/sections/dt2-1.16/section_7_BLOB.bin \
  --name dt2-1.16_SHARC --seed-calls --analyze          # add --overwrite to redo
uv run python tools/sharcldr.py out/sections/dt2-1.16/section_7_BLOB.bin --main out/sharc/dt2-1.16-main.bin
uv run python tools/sharc_disasm.py out/sharc/dt2-1.16-main.bin
uv run python tools/sharccompare.py out/sharc/dt2-1.16-main.bin --json out/sharc/compare-dt2-1.16-new.json
uv run python tools/sharcimm.py out/sharc/dt2-1.16-main.bin --json out/sharc/imm-periph-dt2-1.16.json
uv run python tools/sharcimm.py --words out/sections/dt2-1.16/section_7_BLOB.bin --json out/sharc/words-periph-dt2-1.16.json
uv run python tools/sharcldr.py out/sections/dn2-1.11/section_7_BLOB.bin --main out/sharc/dn2-1.11-main.bin
uv run --with pymupdf python tools/refstext.py            # manuals to out/refs/
uv run python tools/sharcflow.py out/sharc/dt2-1.16-main.bin --program /dt2-1.16_SHARC --cover --analyze   # --save writes
uv run python tools/sharcflow.py out/sharc/dn2-1.11-main.bin --base-sw 0x1c12e2 --program /dn2-1.11_SHARC --cover --analyze

# SHARC: measure a language change, and query a run
uv run python tools/sharcpcode.py measure --out out/sharcpcode/NEW --ghidra   # ~35 s per image
uv run python tools/sharcpcode.py compare out/sharcpcode/t30 out/sharcpcode/NEW
uv run python tools/sharcfields.py                        # declared fields vs the classic grid
sqlite3 -header -column out/sharcpcode/NEW/dt2-1.16.sqlite \
  "ATTACH 'out/sharcpcode/t30/dt2-1.16.sqlite' AS old;" ".read tools/sharcpcode.sql"
uv run python tools/sharcspec/audit_bits.py --top 12     # PRM bits the table does not fix
(cd tools/sharcspec && uv run python build_table.py)     # after editing the merge rules

# ColdFire: dump, Version Tracking check, frame-table map
uv run python tools/ghidradump.py --out out/ghidra/dt2-1.16-emac \
  --project $HOME/ghidra-projects/elektron-emac --project-name elektron-emac \
  --program /dt2-1.16/section_3_MAIN_OS.bin \
  --image out/sections/dt2-1.16/section_3_MAIN_OS.bin --rtti out/symbols/dt2-1.16-rtti.json
uv run python tools/vtcheck.py out/vt/dt2-1.15C_to_dt2-1.16.json \
  --source-image sections/section_3_MAIN_OS.bin --source-dump out/ghidra/dt2-1.15C-emac \
  --dest-image out/sections/dt2-1.16/section_3_MAIN_OS.bin \
  --dest-dump out/ghidra/dt2-1.16-emac --lookup 0x4002d652
uv run python tools/dspmap.py --image out/sections/dt2-1.16/section_3_MAIN_OS.bin \
  --dump out/ghidra/dt2-1.16-emac --json out/maps/dt2-1.16-dspmap.json

# emulator on 1.16: its own sections and snapshots
DT2_SECTIONS=out/sections/dt2-1.16 DT2_SYX=Digitakt_II_OS1.16.syx \
  uv run python tools/sharcframe.py out/snapshots/dt2-1.16/boot400M.snap --passes 3 --open-gate
DT2_SECTIONS=out/sections/dt2-1.16 DT2_SYX=Digitakt_II_OS1.16.syx \
  uv run python tools/bootwatch.py --limit 401000000 --frame-gate --dump out/ghidra/dt2-1.16-emac
```

## Next steps, in order

Put the semantics in the generated language and let Ghidra's analysis draw
function boundaries; do not add boundary heuristics to `tools/sharcflow.py`
(a delay-slot and switch "tidy" step was written and dropped). Work in
`tools/sharcspec/ghidra/gen_sleigh.py`. `tools/ghidra/install-sharc.sh`
regenerates, compiles and installs the language; then
`tools/sharc_import.py ... --overwrite --seed-calls --analyze` and
`tools/sharcflow.py ... --cover --analyze --save` rebuild a program (about
15 s per image; commands under "How to run").

Measure every change with `tools/sharcpcode.py measure --out DIR --ghidra`
and `compare out/sharcpcode/t30 DIR`, which fails on new sleigh diagnostics,
fewer decoded instructions, a conditional branch with no fall-through, a
decompiler failure, a probe that stops passing, or a timing that grows by more
than a quarter. `measure` regenerates the slaspec into a temp directory first
and refuses when the one on disk has drifted from `decode_table.json`, so run
`tools/ghidra/install-sharc.sh` after any table or generator change.

`t30`'s numbers for 1.16: 21,792 aligned instructions, all decoding; 1,169
main-program functions, 95 of them one instruction and 493 two to five; 289
functions truncated at bad instruction data; 160 Error bookmarks; `8a_rel`
branches landing on an instruction start 520 of 590 in range. For 1.11: 21,361
aligned, 1,048 functions, 336 truncated, 178 Error bookmarks. Read the rest
with `tools/sharcpcode.sql` against the run's sqlite dump. Mark unconfirmed
forms unimplemented instead of guessing.

How much p-code exists today: 80 constructors, 28 with a semantic body and 52
empty. Ten of the 52 decoded forms emit any p-code, all of them control flow,
so 19,622 of 1.16's 21,792 aligned instructions lift to nothing. Stage 4 below
covers about 62% of the program by instruction count and stage 6 another 12%.

1. Delay slots in the generator: CJUMP, the return and every delayed
   `8a`/`9a`/`9b` jump, and `11a`/`11c` with j=1 if they compile. The spike
   that proved this compiles is gone (FINDINGS, "Delay slots as one Ghidra
   instruction"); rebuild it from these rules:
   - Two structurally identical slot subtables `s1` and `s2`, each a copy of
     every root constructor with an empty body; one subtable cannot appear
     twice in a pattern. Define them before the constructors that use them.
   - In subtable constructors, quote the mnemonic text (`s1:"jump" ...`):
     bare words there must be operands.
   - A field cannot be both a display operand and constrained (`j=1`) in the
     same constructor: drop it from the display of the split halves.
   - Shapes that compiled: CJUMP (0x1804, 0x1844) as `<pattern> ; s1 ; s2`
     with `build s1; build s2; call target;`; `9a_abs` and `9b_abs` jumps
     split on j, j=1 as `build s1; build s2; return [0:4];` (j=0 unchanged);
     `8a_abs` jump with j=1 as `... goto target;`. RFRAME bodies emptied: it
     restores I7 and I6 and is not a return.
   - A conditional delayed branch must evaluate its condition before the
     slots run. The conditional constructors are already split on cond
     (`gen_sleigh.py`, `conditional_semantics`), so the slots go inside the
     branch that takes it.
   - Remove any test install afterwards: `rm -rf
     /opt/homebrew/Cellar/ghidra/12.1.3/libexec/Ghidra/Processors/SHARC_SPIKE
     ~/ghidra-projects/spike-sharc*`.
   - The probe `returns 2748` is this stage's target. `FUN_001c136a` returns
     through the `9b_abs` at SW `0x1c1496` (`0x083f343f`, the I4/M6 idiom),
     and 2748 = `0xabc` is loaded by the `17b` at `0x1c1498`, in its delay
     slot. No decoder change can make that probe pass; this one can.
2. Attach the ureg register names (SHARC+ Core Programming Reference, UREG
   class table: 0x00-0x0f R, 0x10 I, 0x20 M, 0x30 L, 0x40 B, 0x50 S, 0x60 and
   up system registers) and define the status and system registers (ASTAT,
   STKY, PCSTK, LPSTK, loop registers). The sleigh compiler already runs in
   `tests/test_sharc_pcode.py`.
3. Done in `78a4ee5`, with a correction. `Type8a_abs` and `Type8a_rel` now fix
   bit 25 to zero, and `Type8p_undoc48` takes the 32 words across the two
   images that have it set -- 30 of which point at something that is not an
   address, including the nonsense `jump 0x3e0030` at 1.16 SW `0x1c13bc` and
   1.11 SW `0x1c1366`, the same six bytes in both. 0 regressions, and
   `FUN_001c136a` loses its `halt_baddata()`. But this handover was wrong that
   the function stops there: it does not, and the probe is blocked on stage 1,
   not on the decoder. Restoring the other six dropped PRM bits was measured
   and rejected -- 225 aligned instructions lost in 1.16, 279 in 1.11 -- and
   `RESTORE_PRM_GAP` in `build_table.py` records why, per bit. Still open: what
   the `Type8p` words are (32 across two images, several byte-identical in
   both, so shared code rather than misalignment).
4. P-code for the move and memory forms (`17a`, `17b`, `14a`, `15a`, `15b`,
   `16a`, `3a`-`3c`, `5a`, `5b`, `19a`) and a calling convention (R4, R8,
   R12 in, R0 out, I7 stack, I6 frame).
5. Indirect jumps: `9a`/`9b` jumps go to I + M (pre-modify); only the I4/M6
   jump (raw `0x083f343f`) is a return. The test is the RPC dispatcher's jump
   at `0x1c3c3c` and its cases that jump back to `0x1c3c1d`, and the probe
   "RPC dispatcher is one function", which passes now and must keep passing.
6. ALU and multiplier compute (`4a`, `2c`, `1a`, `1b`) with flags; then the
   condition codes, the shifter and float operations. With real flags, the
   `condition` p-code op the conditional branches call becomes a genuine
   test of ASTAT instead of a placeholder.
7. The provisional forms, when there is evidence: `21p_undoc16` (883 in 1.16,
   969 in 1.11), `23p_undoc16` (407, 399), `22p_undoc48` (91, 129),
   `26p_undoc48` (1, 1). Their prefixes and lengths fit the images; their
   names and semantics are unknown, and the public manuals skip Types 23 and
   24. Twelve of the twenty worst mid-instruction branch targets are still
   unexplained: the instruction the target lands inside decodes cleanly, so a
   neighbouring form is wrong as well.

After stage 4, take up step 1 below (the SPI2 trace) again with the
decompiler.

### 1. Where the SHARC receives the ColdFire's frame

The ColdFire side is known on 1.16 (FINDINGS, "The frame link on Digitakt II
1.16"): the vector-191 handler `0x4002dd0c` builds a frame and calls the
DSPI2 driver `FUN_400cd2bc(0x802, 0x80005348, 0xabc, 0x8000488c)`: it sends
2050 bytes from `0x80005348` and receives 2748 bytes into `0x8000488c`.

Done on 2026-09-15 (FINDINGS, "The SHARC side of the SPI frame link"):
`tools/sharcimm.py` found no SPI, DMA or SEC address in the DSP code; the SPI
bases are in a data table (loader block 35, SPI0/1/2 entries at data
pointers `0x2694a0`/`0x2694c8`/`0x2694f0`, stride `0x28`). SW `0x1c80a0`
passes 2748 to `0x1c7bd4`, which sets R4=2 and R12=`0x261a10` and jumps to
`0x1c9fd5`; that computes I1 = `0x2694a0` + 2 * `0x28`, the SPI2 entry. The
function at `0x1c136a` returns 2748. `0x401` is written to offset `0xc` of a
structure at `0x268220`.

- Follow the SPI2 entry pointer: in `0x1c9fd5` it is I1 after SW `0x1c9ffb`,
  and the calls that follow are `0x1c0cd0` and `0x1c9f9c` (which calls
  `0xb87e25`, outside the loaded blocks). Find where I1 or the struct at
  `0x261a10` is stored and the code that reads the SPI base (entry +0) and
  the DMA bases (+4, +8) from it, then the register writes at base + offset:
  SPI_CTL `+0x04` (MSTR bit 1 says slave or master), RXCTL `+0x08`, TXCTL
  `+0x0c`, RWC `+0x1c`, TWC `+0x24`; DMA CFG `+0x08`, ADDRSTART `+0x04`,
  XCNT `+0x0c`. The offsets are in the table in `tools/sharcimm.py`.
- The small function at SW `0x1ca17a` loads `0x2694f0` and calls
  `0x1c89b9` (bit set/clear helpers); no direct caller was found. Check with
  a raw scan before calling it unused.
- What `0x1c7bd4` does with 2748, and who calls the init run around
  `0x1c80a2`: no `25a_direct` targets `0x1c7ff8`-`0x1c80af`.
- The struct at `0x268220` (+4 = `0x268240`, +8 = `0x100000`, +0xc =
  `0x401`, +0x10 = 4) is passed to `0x1c834a` and `0x1c83ff`: find whether it
  is an SPI buffer descriptor or unrelated.
- These traces read compute instructions by hand. Step 2's flow-override
  pass, and compute p-code, would let Ghidra show them; consider doing it
  before a long trace.
- Follow the receive buffer to its reader: does it end at the RPC dispatcher
  (SW `0x1c35xx`-`0x1c48xx`) or at the command block `0x82a00000`?
- A captured 1.16 frame to match against: `out/sharcframe/dt2-1.16/`
  (`--open-gate`, pass 1, sha256 `d674f76c…`). Its non-zero bytes: offset
  `0x0001` (`0x03`), `0x00d8`-`0x00d9` (`38 40`), single `0x02` bytes at
  `0x011c`, `0x0120` and every `0x60` bytes after to `0x06bc`, `0x06c0`
  (16 pairs, one per track?), `0x0736` (`40`), `0x0738`-`0x0739` (`11 30`),
  `0x07dd` (`08`), `0x07e1` (`12`), `0x07e9` (`02`), `0x07f0`-`0x07f3`
  (`7f ff ff ff`), `0x0801` (`01`). Byte 1 is `0x01` at pass 0 because the
  1.16 installer writes word 1 to `0x80005348`; 1.15C sends the same frame
  with byte 1 = `0x02`.

### 2. How the RPC dispatcher selects a command

Known (FINDINGS, SHARC+ section): the task is created at SW
`0x1c3f5a`-`0x1c3f6a` with entry `0x1c3bf0`; the body starts at `0x1c3bf6`
and calls, through the software-call idiom, `0x1c7d09`, `0x1c7f45`,
`0x1c4353`, `0x1c7e02` and `0x1c43c9` (1.16; 1.15C callees after
`0x1c7700` are `0x6c` lower). All 30 references to the command block
`0x82a00000`-`0x82a001c8` are in SW `0x1c361a`-`0x1c48c3`.

- `tools/sharcflow.py --cover` covers the program (FINDINGS, "The DSP
  programs of Digitakt II 1.16 and Digitone II 1.11 in Ghidra"), but
  function boundaries stay too fine until the language models delay slots
  and indirect jumps (see the plan above). The dispatcher's first piece
  ends in the `9b` jump at `0x1c3c3c`, and its cases jump back to
  `0x1c3c1d`.
- Candidate dispatch jump: `9a_abs` at SW `0x1c46c4`, indirect through I6/M3
  (the return idiom uses I4/M6). Read the instructions before it: what loads
  I6/M3, and is there a table of short-word pointers near it (the old 1.15C
  work labelled 11 RPC handlers with `--label-table ADDR:COUNT`; find the
  table on 1.16)? The `9a_rel` at `0x1c551a` has a fixed target and is not a
  table jump.
- Reading which command field selects the case needs compute semantics:
  `tools/sharcspec/compute_table.json` describes the compute operations; the
  language has p-code only for control flow. Porting compute p-code into
  `tools/sharcspec/ghidra/gen_sleigh.py` is the larger job behind this.

### 3. The Audio Task entry

Created with the same call as the RPC dispatcher: 1.16 at SW `0x1c7775`
(ureg4 = `0x1c7749`, ureg8 = name at `0x2825f7c0`), 1.15C at SW `0x1c7708`
(ureg4 = `0x1c76dc`, ureg8 = `0x2825f7b0`). `0x1c7749` is not an instruction
boundary in our 1.16 decode (a run of `21a`/`22a`/`23p_undoc16` words around
it); on 1.15C `0x1c76dc` decodes as `25a_direct` to `0xb86b1a`, outside the
loaded blocks. Check whether ureg4 is the entry itself or points at a
structure, by comparing with the RPC dispatcher's ureg4 (`0x1c3bf0`, a real
entry).

### 4. What the frame means (ColdFire side)

- Why pass 0 sends no track data, and what the slot words mean. The 1.16
  frame equals 1.15C's except byte 1, and the tables did not move, so this
  can be worked on the 1.15C setup, where the GUI runs.
- Which peripheral raises vector 191: the installer writes priority 5 to
  `ICR1_63` (`0xfc04c07f`), INTC1 source 63. Name it from the MCF5441x
  reference manual (`docs/refs/MCF5441XRM.pdf`, text and tables); it explains
  why the emulator never raises the vector.

### 5. Emulator literals on 1.16 (when a 1.16 GUI run is needed)

- 1.15C literals: `PRINT` and `SWITCH_TO` in `emu/longrun.py`, `TX_STATE`
  `0x4094cd74` and `WAIT_LOOP` in `emu/edma.py`, the weak-pointer patch, and
  the addresses in `emu/panel.py`, `emu/screen.py`, `emu/hle.py`,
  `emu/serial.py`, `emu/uiprobe.py` and the terminal hook in `emu/gui.py`.
  `out/vt/check-dt2-1.15C_to_dt2-1.16.json` gives the 1.16 function for most.
- `emu/symbols.py`: `transport` matches `0x40134200` on 1.16 but the
  counterpart of 1.15C `0x40128c7c` is `0x40136268` (so `call_sites` lists
  25); `ctx_switch_load`'s verify bytes contain `current_tcb` `0x47d9adb4`,
  which moved; the `view_*` and `ui_key_dispatch` hooks are fixed 1.15C
  addresses.

### 6. Still open from 2026-09-14 (1.15C addresses; re-find on 1.16 when resumed)

SLICE-copy gaps (the horizontal line after a trig, `FUN_40017080` type-6
case, unreached shims, a type-read sweep tool); decode the nine descriptor
fields; boot from a patched image (`plan_b` applied to a copy of
`section_3_MAIN_OS.bin`); machine spec file; speed (A/B the idle-spin hook,
idle skipping, button dwell); project save with type 7 and eMMC block storage
in `emu/esdhc.py`; `guirun --regs-at ADDR[=NAME]`.

The 1.15C machine-type runs, if still wanted: three headless runs from
`snapshots/boot400M.snap`, all with `--patch-machine`: ONE SHOT (no
selection, trig only), SLICE, PLACEHOLDER. Add `--save-at
<trig+15M>:out/snap/NAME.snap`, `--watch 0x4094e4ec:16=gate`, `--at
0x40035e90=commit` and PNGs after the trig; then `tools/snapdiff.py` and
`tools/sharcframe.py` on the saved snapshots. Resume a snapshot saved with
`--patch-machine` without passing it again. Runs took 12-23 minutes each.

PLACEHOLDER replay (input times assume `--ips-at 80M:18720000`):
```
uv run python tools/guirun.py snapshots/boot400M.snap --patch-machine --ips-at 80M:18720000 --panel-dwell 12 --at 0x401d5680=cxa_throw --at 0x40035e90=commit --at 0x400caf48=desc --ring 4096 --input 90M:press:17 --input 102M:press:2 --input 102M:release:2 --input 114M:release:17 --input 126M:press:14 --input 126M:release:14 --input 138M:press:14 --input 138M:release:14 --input 150M:press:14 --input 150M:release:14 --input 162M:press:14 --input 162M:release:14 --input 174M:press:14 --input 174M:release:14 --input 186M:press:14 --input 186M:release:14 --input 198M:press:14 --input 198M:release:14 --input 210M:press:10 --input 210M:release:10 --input 238M:press:10 --input 238M:release:10 --input 253M:press:2 --input 253M:release:2 --input 268M:press:25 --input 268M:release:25 --input 283M:press:10 --input 283M:release:10 --png-at 279M:out/after-trig.png --png-at 293M:out/after-yes.png --limit 305000000
```
SLICE: 4 DOWN taps, YES at 190M and 205M, SRC 220M, trig 235M, YES 250M.
Button codes: SRC=2, YES=10, UP=11, NO=12, DOWN=14, FUNC=17, PLAY=20,
TRIG1=25.

## Gotchas

- SHARC addresses: loader byte address = 2 * short-word + `0x28000000` for
  code in the L1 bank. Code pointers in the DSP code (task entries, goto
  targets) are short-word addresses; data pointers (names, tables) are loader
  byte addresses less `0x28000000`, not doubled. In Ghidra the code space has
  wordsize 2: `getAddress(byte_offset)` with byte offset = 2 * short-word,
  and Ghidra prints the short-word address. A wordsize-2 space needs
  `define alignment=2`, or Ghidra silently disassembles nothing.
- In the call idiom the `16a` stores the goto's short-word address minus 1,
  not the address after the call.
- The task-creation call target (`0xb8615d` on 1.16) lies outside every
  loaded section-7 block. Do not read Ghidra's "thunk"/"EXT" labels there as
  evidence of what it is.
- A linear walk over the DSP main program stops at `0xbb0` (2.9% in) with
  either decoder; disassembly has to follow control flow or skip words.
- Em runs things in the same tree. Check `sections/.source-sha256` before
  trusting a run, and never extract into `sections/` or build snapshots in
  `snapshots/`; give 1.16 its own directories through `DT2_SECTIONS` and
  `DT2_SNAPSHOTS`.
- One JVM per Ghidra project. A pyghidra tool under `tools/` must strip its
  own directory from `sys.path` before `import pyghidra`.
- A headless `analyzeHeadless -postScript` runs inside a transaction and the
  analyzer saves the processed program afterwards, even when the script
  fails. Use pyghidra tools (`ghidracopy`, `ghidravt`) for anything that
  must not write.
- Headless `AutoVersionTrackingScript` gets a 2 GB heap and runs out of
  memory on these images; `tools/ghidravt.py` sets 32 GB. Its "apply markup
  errors" are not logged in headless mode.
- Version Tracking matches can be wrong (look-alike virtual methods), and a
  `vfunc_N` slot number can shift between versions. Check with
  `tools/vtcheck.py` and the bytes before relying on a match.
- Ghidra's data references can be wrong: it attributes the DSPI2 driver's
  `move.w a0,(a1,d0.l*4)` to `0x80003340`, while `a1` holds `0x80001bc0`.
  Its call and reference tables also miss code outside functions; confirm
  with `tools/refscan.py`.
- The stock ColdFire language stops at `movclr`; use a ColdfireEMAC import.
- `dt2/elz.py` on 1.16 and 1.11 is checked by internal consistency, not by
  an independent oracle.
- `tools/sharcspec/` scripts run from their own directory and read the PDFs
  from `docs/refs/`.
- Never name the vendor DSP toolchain in the repo; sharc-spec's original
  docs do, the imported copies do not.
- `jcmd` cannot attach to the JVM inside a pyghidra process.
- The coder agent's file tools drop whitespace-only lines: copy patch files
  with `cp`, and check a patch's SHA-256 after writing it.
- Subagents return only their final message and can stop at a turn limit on
  long edit lists; split large edits.
- Shell is zsh (an unquoted `$VAR` is one word), there is no `timeout`
  binary, and the rtk hook shortens `git log` and `ls` and rejects
  `find -newer` (use `rtk proxy`).
- Every SHARC+ CJUMP (`25a`) is a delayed call: the push of R2 and the store
  of the return address - 1 after it are its delay slots, and the return
  lands after them. Returns and other delayed jumps also run the two
  instructions after them. Read listings with that in mind.
- SLEIGH's `delayslot(n)` counts bytes, so it cannot model two SHARC+
  delay-slot instructions of varying length (Next steps, stage 1).
  `getMaximumInstructionLength()` is empty for the generated language, and
  18-byte instructions work.
- Manual text is in `out/refs/<pdf stem>/` (`toc.md`, `pages/pNNNN.txt`,
  `all.txt`) from `tools/refstext.py`: grep there, do not extract the PDFs
  again.
- Em wants strict delegation: scout reads, coder edits, general-purpose
  agents run every process, including short commands and git.
- Analyse Digitakt II 1.16 and Digitone II 1.11 only; use 1.15C only when a
  hardware test needs it.
- A Ghidra JVM holds one version of a language for its lifetime, so a project
  read after a different language was installed gives wrong numbers. Measure
  each language in its own `tools/sharcpcode.py` run.
- `tools/sharcspec/build_table.py` has no `if __name__` guard: importing it
  rewrites `decode_table.json`. Copy what you need from it, as
  `audit_bits.py` does.
- The generated `tools/sharcspec/ghidra/SHARC_VISA/` tree is git-ignored and
  nothing regenerates it on its own, so a decode-table edit leaves it stale
  with no sign. That cost four measurements on 2026-09-16. The installed-
  language check could not catch it: a stale slaspec compiles to the stale
  `.sla` that is installed, the two agree, and the run proceeds. `measure` now
  regenerates into a temp directory and refuses on drift, and records
  `decode_table_sha256` in `lint.json`. When a change measures perfectly flat,
  check that hash before believing it.
- `tools/sharcfields.py` is the mirror of `audit_bits.py`: it tallies what
  values the table's declared fields actually take in the firmware and flags
  any field sitting on bits the classic grid fixes. It reads `classic_keys`,
  which `build_table.py` now records per form.
- A form merged from two classic tables has a real field wherever those tables
  disagree, and reading only one of them makes that bit look like a conflict.
  Guessing the classic table from the form name gets every split form wrong:
  `Type8a_rel` guesses `Type 8a` where the merge used `Type 8a #2`. Both
  mistakes were made and both produced confident false findings.

## Workflow

scout reads, coder applies fully specified edits, general-purpose agents run
processes, Ghidra and the emulator. Record results in `docs/FINDINGS.md`,
and have a second agent check a finding against the bytes before marking it
**[V]**. Commit when Em asks; Em asked for a commit and push at each clean
breaking point.
