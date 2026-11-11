# __TITLE__

__ID__: a SHARC+ effect for the Digitakt II and the Digitone II, loaded at run time. It scales the master block by one
GAIN knob. Replace the loop in `src/dsp/__NAME__.s` with your own processing.

- `module.toml`: the module and its objects.
- `src/module.c`: the effect page and its knob, declared with `DK_FX` and `DK_KNOB` (`<dk/fx/fx.h>`).
  `patchkit desc __ID__` prints what it declares.
- `src/__NAME__.h`: the knob word and the knob count, shared by `src/module.c` and the assembly.
- `src/dsp/__NAME__.s`: the process routine and the descriptor (`<dk/fx/fx.h>`).

```sh
mise run module:check -- __ID__    # build and conformance check on both devices
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
