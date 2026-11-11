# __TITLE__

__ID__: a ColdFire service module in C, loaded at run time. It logs "hello" when the runtime loads it. Add your code to
`src/c/__NAME__.c`.

- `module.toml`: the module and its C object (`place = "cf.expansion"`).
- `src/c/__NAME__.c`: `dkm_init` and `dkm_fini` (`<dk/core/api.h>`). To add SysEx commands, allocate a range in `sdk/sdk.toml`
  `[protocol.ranges]` and call `api->register_cmds` in `dkm_init`.

```sh
mise run module:check -- __ID__    # build and conformance check
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
