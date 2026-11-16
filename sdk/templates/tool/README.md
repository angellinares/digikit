# __TITLE__

__ID__: a ColdFire tool module in C (a run-time module without machines or effects; `type = "service"` is the manifest type of these modules), loaded at run time. It logs "hello" when the runtime loads it. Add your code to
`src/c/__NAME__.c`.

- `module.toml`: the module and its C object (`place = "cf.expansion"`).
- `src/c/__NAME__.c`: the entry point `dk_module` with `init`, `fini` and `tick` (`<dk/core/module.h>`). To add SysEx commands, allocate a range in
  `sdk/sdk.toml` `[protocol.ranges]` and call `dk_host_register` in `init`.

A service file (`role = "service"`, a `.dks` that owns record kinds and takes hooks) is another thing: start it from `sdk/templates/service` (`mise run module:new -- service NAME`).

```sh
mise run module:check -- __ID__    # build and conformance check
mise run module:pack -- __ID__ --device dt2     # make the .dkm (or --device dn2)
mise run dk:run -- __ID__ --emu --seconds 10    # run in the running emulator (mise run workbench)
```
