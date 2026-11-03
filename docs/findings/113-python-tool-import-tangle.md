# Python tool import tangle (2026-11-02)

**[D]** Measured once on `3077af34` with a scan of module-level and function-level
`from tools.X` / `import tools.X` statements in `tools/**/*.py`.

The eight Python tool owners under `tools/` form one strongly connected import graph: every owner
reaches every other one.

| Owner | Imports owners |
| --- | --- |
| coldfire | control, device, emulator, firmware, patchkit |
| control | emulator |
| device | control, emulator, firmware, patchkit |
| devtools | control, device, emulator, firmware, patchkit |
| emulator | coldfire, control, devtools, firmware, patchkit, sharc_dev |
| firmware | coldfire, control, emulator, patchkit |
| patchkit | coldfire, control, device, devtools, emulator, firmware, sharc_dev |
| sharc_dev | coldfire, devtools, emulator, firmware, patchkit |

Twelve edges rest on one or two import statements each: control→emulator, device→emulator,
device→firmware, devtools→device, devtools→firmware, emulator→coldfire, firmware→coldfire,
firmware→control, firmware→emulator, sharc_dev→devtools, sharc_dev→firmware, sharc_dev→patchkit.

Consequence for test selection: an Nx project per owner (or per `tests/<dir>`) would make any tool
change select every Python test directory. The gate therefore keeps per-file import selection for
Python tests (`tools/devtools/changed_tests.py`). Selection by owner, and caching of Python test
results by owner, need these cycles removed first (**[O]**).
