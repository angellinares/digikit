# Run-time effects (core/dynfx)

Reserves the DSP side of effects that the digikit runtime loads at run time: one free row in the effect chain of `core/fx` for each effect slot of the device
(`[limits] fx_slots`; 3 on both devices) and a table of their addresses (`dynfx_info`). It changes nothing by itself: a free row has pc `0xffffffff`, which the adapter
skips, so the chain stays empty and the audio is the stock audio. Needs `core/runtime`, `core/dsp-link`, `core/dyndsp` (the DSP area) and `core/fx` (added automatically).
Digitakt II 1.17 and Digitone II 1.12, emulator only. On the Digitone II it brings `core/fx` into the image; the emulator bars only the blocks the hook changes, so it costs x1.01 DSP CPU there (finding 55), and it is in both DN2 patch sets.

How it works and its limits: `docs/design/digikit-dkm.md` ("DSP section", effect records), `docs/findings/55-fx-adapter.md` ("Run-time rows"). The runtime claims a free row when it
reserves the module's DSP range and publishes it, one word per transfer through window 2 of `core/dsp-link`, the pc word last. Test module: `test/dyn-fx-gain`; proof:
`modules/core/adapters/dynfx/tests/fx-chain.device.test.ts`.
