# Contributing

## Before you start

Read [README.md](README.md) (setup), [docs/LAYOUT.md](docs/LAYOUT.md) (who owns what),
[docs/TESTING.md](docs/TESTING.md) (tests and commands) and
[docs/MODULE-AUTHORING.md](docs/MODULE-AUTHORING.md) (modules).

## Never commit

- Firmware (`*.syx`) or anything derived from it: `firmware/` holds the images and
  is ignored; `out/` (sections, snapshots), `apps/web/.local`, card images.
- Users' sample audio.
- Secrets.

A module never contains firmware bytes ([modules/README.md](modules/README.md)). Cite only
public manuals for the SHARC+ ([docs/sharc/SOURCES.md](docs/sharc/SOURCES.md)). Do not name,
copy or quote the vendor DSP toolchain or its files.

## Results

Reverse-engineering results go in `docs/findings/`, one file per topic, indexed from
[docs/FINDINGS.md](docs/FINDINGS.md). Marks: **[V]** verified here, **[D]** measured or
read once, **[O]** open, **[C]** corrects an earlier claim. A second person checks a
finding against the image bytes before it is **[V]**. Handovers (`docs/handovers/`) carry
state and next steps, not results.

## Branches and pull requests

- Work on a branch. `main` takes changes through a pull request; you merge your own PR once its review
  threads are resolved (squash or merge commit). `main` cannot be force-pushed or deleted.
- Keep moves and renames, helper extraction and behaviour changes in separate commits.
- Keep the PR description short, in plain technical English. Say what was verified.

## Checks

- While you work, run the tests for the code you changed (pytest on the test file; see
  [docs/TESTING.md](docs/TESTING.md)).
- Before a PR, run `mise run gate`. It selects the Nx projects your changes affect;
  `pnpm nx:pilot show projects --affected --base main` shows the selection.
- `mise run gate` is also the gate for merging to `main`. Run `mise run gate:full` only after changes to the
  generated SHARC core.
- New Python files are covered by ruff through `[tool.ruff] include` in `pyproject.toml`. Add typed files to
  `[tool.mypy] files` in `pyproject.toml` where practical.
- Python tools go in the owner's package under `tools/` (docs/LAYOUT.md); run them with `uv run python -m tools.<owner>.<module>` and never change `sys.path`.
- PCM pins in `tests/scenarios/*.json` and the SHARC golden hashes change only with the
  maintainer's approval and a stated reason.

## Hardware

No patched firmware has been flashed to a device. Do not flash it. Hardware tests come only
after the emulator shows that the change works.

## Agents

[AGENTS.md](AGENTS.md) holds the same rules and the agent workflow (lanes, roles).

## License

AGPL-3.0-or-later with additional terms in [NOTICE](NOTICE). See "Licence" in
[README.md](README.md).
