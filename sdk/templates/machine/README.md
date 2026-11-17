# __TITLE__

__ID__: a SHARC+ voice machine for the Digitakt II and the Digitone II, loaded at run time. It plays a pitched sine with one
level parameter. Replace the render loop in `src/dsp/__NAME__.s` with your own voice.

- `module.toml`: the module, its machine id and its objects.
- `src/module.c`: the machine, its page and its parameter, built in `init` with `dk_module_ctx_add_machine`, `dk_machine_add_page` and `dk_page_add_param`.
- `src/dsp/__NAME__.s`: `render`, `note_on` and the descriptor (`<dk/gen/machine.h>`).

`src/module.c` shows `dk_param_range` and a second page as comments. The machine needs `machines.dks` on the card: the loader finds it through the NEED of the builder imports `init`
calls, and Transfer installs it. `requires = []` is right: it names patched-in core modules only. `module:check` writes the `[uses.*]` lines, `record_bytes` included.

```sh
mise run module:check -- __ID__    # build and conformance check on both devices
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
