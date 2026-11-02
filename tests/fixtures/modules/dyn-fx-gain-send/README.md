# FX gain send, run time (test)

The same effect as `test/dyn-fx-gain` with `route = "send"` in its `DK_FX` record (`src/module.c`): it starts on the send route and may be switched to the master (`routes` defaults to `["send", "master"]`). Used by `modules/core/adapters/fx/tests/fx-routes-panel.device.test.ts` and `modules/core/adapters/fx/tests/fx-routes-persist.device.test.ts` on both devices.
