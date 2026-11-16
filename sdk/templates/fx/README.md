# __TITLE__

__ID__: a SHARC+ effect for the Digitakt II and the Digitone II, loaded at run time. It scales the master block by one
GAIN knob. Replace the loop in `src/dsp/__NAME__.s` with your own processing.

- `module.toml`: the module, its effect id and its objects.
- `src/module.c`: the effect, its page and its knob, built in `init` with `dk_module_ctx_add_fx`, `dk_fx_add_page` and `dk_page_add_fx_param`.
- `src/__NAME__.h`: the knob word and the knob count, for the assembly.
- `src/dsp/__NAME__.s`: the process routine and the descriptor (`<dk/gen/fx.h>`).

`init` also sets the routes the effect may run on (`dk_fx_routes`: master, send or both, and the default) and the default of the MIX knob (`dk_fx_mix`); a signed knob uses `DK_BIPOLAR`
(a commented example is in `src/module.c`).

The effect needs `fx.dks` on the card: the loader finds it through the NEED of the builder imports `init` calls, and Transfer installs it. `requires = []` is right: it names
patched-in core modules only. `module:check` writes the `[uses.*]` lines, `record_bytes` included.

```sh
mise run module:check -- __ID__    # build and conformance check on both devices
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
