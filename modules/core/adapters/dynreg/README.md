# Run-time registry (core/dynreg)

Reserves machine, parameter and (Digitone II) page rows that modules loaded at run time by the digikit runtime fill: 8 machine types (16 to 23),
16 parameter codes and, on the Digitone II, 8 page rows. It changes nothing by itself: the reserved rows read as gap rows, and the machine list
leaves them out until a module has filled them. Needs `core/runtime`, `core/machines` and `core/params` (added automatically). Digitakt II OS 1.17
and Digitone II OS 1.12, emulator only.

The machine-view hook and the three in-use anchors of hot reload are archived (`docs/plans/digikit-hot-reload.md`).

How it works and its limits: `docs/design/digikit-registry.md`. Test modules: `test/dyn-xslice`, `test/dyn-dn2` (both devices). Anchor rules: declared in `devices/fw/machine.toml`.
