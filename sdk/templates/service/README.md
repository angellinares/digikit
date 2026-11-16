# __NAME__ service

A service crate of the digikit runtime (`modules/core/services/__NAME__`), made by `mise run module:new -- service __NAME__`.

Register it, then check it:

1. Add the crate to the workspace members of `Cargo.toml` and to the runtime's dependencies (`modules/core/runtime/src/rs/Cargo.toml`).
2. Add a `[[service]]` row (`rust = "dkrt_svc___NAME__::records::__CAMEL__"`) and its `[records.kinds]` entries to `sdk/sdk.toml`, then `patchkit gen`.
3. Add the owner to `modules/core/runtime/owners.toml` (`"S:__NAME__" = ["dkrt_svc___NAME__"]`).
4. `mise run module:check -- service/__NAME__` checks the layout, the one exported entry and the declarations against `sdk.toml`, and runs the crate's tests.
