# test/dyn-demo-stray

A test fixture for the ColdFire memory audit (`mise run module:audit`), not for players. The same module as `test/dyn-demo`, but slot
call number 500 also writes one word 128 KiB above the module's counter: in the arena, outside the module's own block. The negative control in
`modules/core/runtime/tests/memaudit.device.test.ts` must report exactly this one store, with this module's code as the writer. Emulator only.
