# digitakt2

Reverse engineering of the Elektron Digitakt II firmware: a ColdFire MCF5441x
main CPU and a SHARC+ DSP. The work is moving from OS 1.15C to 1.16; the
device stays on 1.15C (installing 1.16 upgrades the bootstrap irreversibly). Setup is in README.md, results
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

## Shell and tests

- Tests: `uv run --with pytest python -m pytest tests -q`. It skips tests
  marked `slow` (long firmware integration runs); add `--slow` before a
  commit. While working, run only the test files for the code you changed.
- The shell is zsh: an unquoted `$VAR` is one word, not split. There is no
  `timeout` binary.
- The rtk hook shortens some output: use `rtk proxy git log` for the full log.
- Extract any firmware, 1.16 included: `uv run python -m emu.extract SYX -o DIR`
  (`dt2/elz.py`; `--oracle` uses the device routine, 1.15C/1.10E only).
- Manuals: read `out/refs/<pdf stem>/` (`toc.md` bookmarks with pages,
  `pages/pNNNN.txt`, `all.txt` for grep), not the PDFs. Build it with
  `uv run --with pymupdf python tools/refstext.py`; `--render PDF PAGE` makes
  a PNG of one page for figures.
