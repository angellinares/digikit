# FX hang (test)

A run-time effect (Digitakt II and Digitone II) on the send route whose process routine stores the steps 1 and 2 in `_dk_step` and, from its 5000th call (about 3 s of blocks after it starts)
on, step 3 and an endless loop. The late start keeps the stop after the second poll of the heartbeat watch, so the watch declares it after 3 s of frozen blocks (not 20 s). It shows what the fault trace reports when the SHARC stops inside an effect: the next boot is safe mode (`DSP STOPPED`),
ABOUT names the module, `FX SEND` and `STEP 2` (the step of the last block that ended; the step 3 of the stopping call is lost with the core), and `dk/fault` holds the record. Used by `modules/core/runtime/tests/fault-trace.device.test.ts`.
Pack it with `patchkit pack test/dyn-fx-hang --device dt2 -o fx-hang.dkm`. Do not select it on a real device: the DSP stays stopped until the restart.
