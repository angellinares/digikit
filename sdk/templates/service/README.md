# __NAME__ service

A service of the digikit runtime, made by `mise run module:new -- service __NAME__`. Every service has this shape:

```text
modules/core/services/__NAME__/
  Cargo.toml        package dkrt-svc-__KEBAB__: the neutral crate, host-tested over FakeCore
  src/lib.rs        pub mod records, slots (and dsp when there is a src/dsp/ folder)
  src/records.rs    the Service handlers (declare, check, apply, remove, tick ...)
  src/slots.rs      what the service fills in the firmware (rows, slots, hooks)
  src/dsp/          the service's parts of the DSP section (only when it has any)
  tests/            host tests over FakeCore
  dks/module.toml   id __ID__, role = "service"; one [[hook]] for each hook of service!
  dks/src/lib.rs    the service!(...) call and nothing else
  dks/src/hooks.rs  every hook as assembler (only when the service takes hooks)
  dks/src/fw.rs     firmware calls (only when the service needs them)
```

The neutral crate exports only `dk_module` and `DKRT_*` state symbols, has no assembler and names no firmware (no `cf.` hook or anchor names, no `dkfw`). What the service
needs from the image comes from the image adapter `modules/core/adapters/X`: reserved rows, hook stubs and `[[slot]]` words, and `[[info]]` data tables, found by name hash
with `dkc_info` (`CoreApi::info`). The image half is the adapter's; the runtime names neither the service nor the adapter's table.

Make it a real service:

1. Add a `[[service]]` row (no `rust`: the service is the file `__NAME__.dks`) and its `[records.kinds]` entries (`service = "__NAME__"`) to `sdk/sdk.toml`, then `patchkit gen`.
2. Add the workspace member in `Cargo.toml`. A service file needs no entry in the runtime's dependencies or `owners.toml`.
3. A service that takes hooks: one `[[hook]]` in `dks/module.toml` for each, the `service!(..., setup = f, hooks = [("cf.x.hook.y", hooks::entry), ("cf.x.hook.z", hooks::z_dn2, dn2)])` list (an entry with `dt2` or `dn2` is for that device),
   and the entry as assembler in `dks/src/hooks.rs`. A hook that takes the whole control flow of its site (no handler in `devices/fw/`) may continue in the stock code of the site with the exit `tail = true`;
   its assembler refers to `dk_tail_<8 hex digits of name_hash(hook)>`, which `patchkit bind` resolves to code generated from the user's firmware (docs/PATCHING.md "Raw hooks").
   A fill (the pointer slot of a fixed firmware site) is one `[[fill]]` (`slot`, `entry`, optional `devices`) in `dks/module.toml` and one `fills = [("slot", hooks::fill_fn, dn2)]` entry;
   as for hooks, an entry with `dt2` or `dn2` is for that device. The long forms of `service!` need `setup = f` (the image half), and `fills` and `exports` must match their `[[fill]]` and `[[export]]` entries.
   `dks/module.toml` and `dks/src/lib.rs` hold commented examples of each.
   A service that needs an image table (like `machines_info`) requires the adapter that provides it, as `core/services/machines` requires `core/machines`: put the adapter in `requires` of `dks/module.toml`.
   A service that offers functions to other modules: one `[[export]]` in `dks/module.toml` for each and `exports = [("dk_name", fw::dk_name)]` after `fills =` (`hooks =` and `fills =` come first; pass `fills = []` when there is none). `service!` declares each export in `init`:
   a header entry `init` does not declare refuses the file with `E_DECL_KIND`.
4. `mise run module:check -- service/__NAME__` runs `patchkit service-check` (the layout above, the `service!` hooks against `[[hook]]` per device, assembler only in `dks/src/hooks.rs`, firmware names only under `dks/`,
   the one exported entry, the declarations against `sdk.toml`) and the crate's tests.
