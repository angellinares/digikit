# digitakt2

Reverse engineering of the Elektron Digitakt II firmware: a ColdFire MCF5441x
main CPU and a SHARC+ DSP. Targets are Digitakt II OS 1.16 and Digitone II
OS 1.11; the device runs 1.16. Setup is in README.md, results
in docs/findings/ (one file per topic, indexed from docs/FINDINGS.md), and
current state and next steps in the newest `HANDOVER-*.md` in the repo root.

## Rules

- Record results in docs/findings/ (indexed from docs/FINDINGS.md), not in handovers. Marks: **[V]**
  verified here, **[D]** documented or read once but not re-checked, **[O]**
  open, **[C]** corrects an earlier claim. Have a second agent check a
  finding against the image bytes before marking it [V]. An empty Ghidra
  caller list is not evidence of dead code.
- Firmware and anything derived from it (`*.syx`, `sections/`, `out/`,
  `snapshots/`) is Elektron's copyright: never commit it.
- Never name, copy or quote the vendor DSP toolchain or its files in
  commits, docs or code. Cite only public manuals; SHARC+ sources are listed
  in `docs/sharc/SOURCES.md`.
- Commit only when Em asks.
- Run the emulator only when a static answer is not enough, and bound runs
  with `--limit`. Em runs emulator commands in the same tree:
  `sections/.source-sha256` is the SHA-256 of the .syx the sections came
  from, so compare it with `shasum -a 256 <the .syx>` before trusting a run.

## Agents

- scout reads code. Only its final message returns, so ask for quotes there.
- coder applies fully specified edits (exact before/after text or full file
  content). Check `git diff` afterwards.
- general-purpose agents run processes: tests, Ghidra, the emulator.
- An agent in a git worktree runs `tools/worktree-setup.sh` first: it
  fast-forwards to `work/sharc-emulator` and links `out/`, `sections/`,
  `snapshots/`, `.venv` and the generated language files from the main tree.
  Do not change dependencies there (the virtualenv is shared).

## Ghidra

- Analysis project: `~/ghidra-projects/dt2-emac` (project name `dt2-emac`,
  program `/section_3_MAIN_OS.bin`, language `68000:BE:32:ColdfireEMAC`).
  The stock ColdFire language in `~/ghidra-projects/dt2` stops decoding at
  `movclr`, so do not use it near interrupt handlers.
- One JVM per project at a time. Read the dump first:
  `out/ghidra/dt2-1.15C-emac/` from `tools/ghidradump.py` (`rg` over
  `decomp/`, `sqlite3 xrefs.sqlite`; check `complete` and `image_sha256` in
  `manifest.json`). For live queries use `tools/ghidraq.py` with
  `--project ~/ghidra-projects/dt2-emac --project-name dt2-emac`, chaining
  queries with `--then` in one JVM.
- Ghidra's call and reference tables miss code outside functions, such as
  small trampolines. Before recording "no caller" or "no writer", confirm
  with `tools/refscan.py` on the raw image.
- A pyghidra tool under `tools/` must remove its own directory from
  `sys.path` before `import pyghidra` (copy the block in `tools/ghidraq.py`):
  `tools/ghidra/` shadows the `ghidra` package, and the import fails with
  `RecursionError`.
- After a Ghidra upgrade, re-run `tools/ghidra/install-coldfire-emac.sh`.
- SHARC+ encodings: `tools/sharcspec/decode_table.json` and
  `compute_table.json`, built from the public ADI manuals
  (`tools/sharcspec/README.md`).
- Measure the generated SHARC+ language before and after a change:
  `tools/sharcpcode.py measure --out DIR [--ghidra]`, then `compare OLD NEW`
  (sleigh diagnostics, the pypcode lift, Ghidra analysis, decompiler and
  probes). Each run writes `DIR/<image>.sqlite` with our decoder's and
  Ghidra's view of a program: query it with `sqlite3` and
  `tools/sharcpcode.sql` instead of writing another pyghidra script. One JVM
  holds one version of a language, so reading an old project after installing
  a new one gives wrong numbers; measure each language in its own run.
- SHARC facts come from the program database first:
  `uv run python tools/sharcdb.py build out/sections/*/section_7_BLOB.bin`
  (seconds; skipped when current) writes `out/sharcdb/<image>.sqlite`:
  instructions, functions, edges, basic blocks, literals, memory accesses,
  data references, register def/use, cross-image function hashes, and a
  whole-image analysis (roots, reach, call graph, dominators, loops,
  unentered functions). Use it through `tools/sharc.py`: in one script,
  `img = sharc.load("dt2-1.16")` then `img.func`, `callers`, `callees`,
  `reach`, `roots`, `last_def`, `uses`, `refs`, `xref_table`, `match`,
  `trace`, or `img.sql(...)`; `uv run python tools/sharc.py IMAGE "SQL"` for
  one query. Do this before running `tools/sharcfn.py` or writing a script.
  A new kind of fact goes into `tools/sharcdb.py`, not a scratch script.
- ColdFire facts (MAIN OS) work the same way, from a `tools/ghidradump.py`
  dump: `uv run python tools/cfdb.py build out/ghidra/dt2-1.16-emac` writes
  `out/cfdb/<image>.sqlite` (functions, call/jump edges, data references,
  strings; skipped when current). Use it through `tools/cf.py`:
  `img = cf.load("dt2-1.16-emac")` then `img.func` (by address or name),
  `callers`, `callees`, `reach`, `paths`, `refs_to`, `decomp`, or
  `img.sql(...)`. `tools/cf_names.py` holds hand-curated 1.16 address names
  (live kit/track records, FlexBus, sample loader, boot-time UI) that both
  `tools/cf.py` and `tools/snapread.py` resolve against.
- `tools/snapread.py` reads registers/memory/task state from an
  `emu/snapshot.py` `.snap` file without restoring a Unicorn machine:
  `Snapshot(path).u32(addr)`/`.read(addr, n)`/`.regs`/`.tasks()`, or the CLI
  (`read`/`u32`/`u16`/`u8`/`regs`/`tasks`) -- for read-only questions where
  spinning up a Machine just to inspect a capture would be wasted work.
- SHARC+ instruction semantics live once, in `tools/sharc_core/` (layered
  modules; forms dispatch through `sharc_core.forms.FORMS`).
  `tools/sharc_trace.py` is the symbolic driver and `tools/sharc_run.py` the
  concrete one; neither holds semantics of its own.

## Shell and tests

- Tests: `uv run python -m pytest tests -q`. It skips tests
  marked `slow` (long firmware integration runs); add `--slow` before a
  commit. While working, run only the test files for the code you changed.
- Lint: rules are in `pyproject.toml`. `tests/test_lint.py` runs `ruff check`
  and `ruff format --check` on its `CLEAN` list; new files start clean and go
  on the list, and a file joins it when you clean it.
- Behaviour: `tests/test_sharc_golden.py` hashes six SHARC core outputs on
  DT2 1.16. A refactor keeps them; an intended change updates them with
  `uv run python tests/test_sharc_golden.py --update` and says why.
- Types: `tests/test_types.py` runs mypy (config in `pyproject.toml`, checked
  as Python 3.11) on its `TYPED` list. The SHARC core must also run under
  PyPy 3.11: `tests/test_pypy.py` (slow) runs its tests there.
- The shell is zsh: an unquoted `$VAR` is one word, not split. There is no
  `timeout` binary.
- The rtk hook shortens some output: use `rtk proxy git log` for the full log.
- Extract any firmware, 1.16 included: `uv run python -m emu.extract SYX -o DIR`
  (`dt2/elz.py`; `--oracle` uses the device routine, 1.15C/1.10E only).
- Manuals: read `out/refs/<pdf stem>/` (`toc.md` bookmarks with pages,
  `pages/pNNNN.txt`, `all.txt` for grep), not the PDFs. Build it with
  `uv run --with pymupdf python tools/refstext.py`; `--render PDF PAGE` makes
  a PNG of one page for figures.
