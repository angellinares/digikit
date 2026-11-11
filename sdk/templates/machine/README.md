# __TITLE__

__ID__: a SHARC+ voice machine for the Digitakt II and the Digitone II, loaded at run time. It plays a pitched sine with one
level parameter. Replace the render loop in `src/dsp/__NAME__.s` with your own voice.

- `module.toml`: the module and its objects.
- `src/module.c`: the machine, its parameter and its page (`DK_MACHINE`, `DK_PARAM`, `DK_PAGE` of `<dk/machine/machine.h>`).
- `src/dsp/__NAME__.s`: `render`, `note_on` and the descriptor (`<dk/machine/machine.h>`).

```sh
mise run module:check -- __ID__    # build and conformance check on both devices
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
