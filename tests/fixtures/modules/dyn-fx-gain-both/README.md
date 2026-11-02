# FX gain both, run time (test)

The same effect and page as `test/dyn-fx-gain` (tab GAN) with `route = "master"` and `routes = ["master", "send"]` in its `DK_FX` record (`src/module.c`): an insert by default that the settings key `fx/route/<module id>` can move to a send. Used by the track send-page suite `modules/core/adapters/fxpages/tests/fx-track-route.device.test.ts` on both devices (the manifest before the route fields could not be a send effect without the key: `test/dyn-fx-gain` allows the master only now).
