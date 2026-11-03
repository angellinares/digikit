# Public listing boundary fixture

This module uses only `<dk/files.h>` and public service commands. It registers no
slots, so deferred notifications do not require slot 0. API 1.4 / runtime 0.5.4,
304-byte entry offsets and the old API prefix are checked by its CF compilation.

Commands (all integers in reply bodies are big endian):

- `0x38`: root byte, mode byte, NUL path. Returns ticket. Mode 1 polls in the
  deferred callback; mode 3 also cancels reentrantly after its first row; mode 5
  records only a readiness hint for manual capacity/consumption checks.
- `0x39`: client, current ticket, total retained row count, terminal status,
  callback count, last hint for current ticket, previous ticket. May return BUSY
  if a callback overlaps its bounded seqlock observation.
- `0x3a`: one-byte row index; returns its ticket plus the exact 304-byte owned row.
- `0x3b`: ticket; returns the signed cancellation result.
- `0x3c`: ticket plus invalid-capacity byte; returns signed poll result and an
  unchanged-output flag, followed by a row only on result 1.
- `0x3d`: API size, runtime version, entry size, client-getter result outside init.

Callback rows append to a fixed 32-row log, never overwritten/reset by a later
request. Ticket tags handle callbacks that run before the MIDI list caller has
recorded its returned ticket. Context is static module storage. Overflow cancels
with TOO_BIG. This is a bounded diagnostic, not a production consumer/cache.

Prepare with the content-addressed patchkit built for the current SDK, then
`patchkit pack test/files-proof --device dt2|dn2 -o .../files-proof.dkm`.
The focused native case is `tests/regression/runtime/files-api.test.ts`. It needs
prepared daemon, runtime-only patched SYX/plan, DKM artifacts and a DT2 card image;
it never builds, applies a patch, or calls card.load. Environment variables are
`DIGI_FILES_EMUD_DT2/DN2`, `DIGI_FILES_PATCH_DT2/DN2`,
`DIGI_FILES_PLAN_DT2/DN2`, `DIGI_FILES_CARD_DT2`, `DIGI_PATCHKIT_BIN` and optional
`DIGI_RUNTIME_MODULES`. Use standard ready inputs, not the private LP0 fixture.

Live coverage passes on DN2 1.12 and DT2 1.17; see
[finding 93](../../../../docs/findings/93-sdk-file-catalogue.md) for artifacts and limits.
Pure broker tests cover callback
replacement/list reentrancy and retirement; this first native fixture exercises
callback polling/cancel, not reentrant list or failed-init teardown.
