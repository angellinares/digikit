# FX NaN (run time, test)

A run-time send effect whose process routine writes a NaN into its output block. `core/fx` counts a fault in every block and switches the row off
after the eighth within 1500 blocks. The debug log shows `FX_FAULT` and `FX_DISABLED` with the source of this module (`modules/core/runtime/tests/fault-trace.device.test.ts`).
