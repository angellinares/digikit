# Reusable SDK file listing

Public implementation and DN2/DT2 emulator acceptance, 2026-10-30. Scope: read-only file and directory listing for
the sampler and a later file manager. This does not define sample-bank ownership
or file mutation operations. See the [implementation plan](../plans/dsp-sdk-and-sampler.md)
and [provider evidence](../findings/93-sdk-file-catalogue.md).

## Decisions

- Listing is a runtime SDK service. Modules use the same public interface on both
  devices; provider-specific firmware calls remain behind adapters.
- Roots are explicit: samples (DT2 +Drive, DN2 `samples/`) and the runtime store.
  Paths are relative to a root, complete and NUL terminated at the C boundary.
  Empty path means that root. A row identity includes the captured root and full
  path; a displayed label or row position is never an identity.
- Return file/directory type, source byte length where available, and typed stored
  hash metadata. DN2 SHA-256 and DT2's stored odd hash are different algorithms.
  Neither is recomputed by listing. Size means bytes available from the source;
  DT2 stock ReadDir subtracts a 16-byte trailer, so its displayed size is not that
  source length. Unknown metadata must be distinguished from zero.
- Directory listing is incremental, with explicit pending, data, end and error
  states. Polling a result does no storage I/O. Cancellation or another folder
  request invalidates previous results immediately, using request generations.
  Poll copies a row into caller-owned memory; it never lends service storage.
  Copy and consumption are synchronized with replacement. A later request cannot
  overwrite a row already returned to its caller.
- Acquire a module-owned client handle during initialization. UI callbacks do not
  have reliable `dlog::current_source` attribution; they retain and use that handle.
  Module teardown invalidates it. Handle/request counters must not wrap into a
  previously valid value during a session.
- Queue copies are separate from active provider buffers. A MIDI callback can
  replace a request while a UI-task firmware call is in progress; that call must
  keep its original path/state alive and its late result must not publish.
- Storage work and completion callbacks run outside UI fill/action callbacks.
  Polling alone must not require a module to own slot 0 (that slot has one owner).
  A completion notification can let the module refresh its existing SDK menu.
  Call notifications outside critical sections, with their client/request identity;
  reentrant cancel/new-request calls must be supported.
- Notifications are deferred readiness edges, never synchronous calls from
  begin/poll/cancel. Dispatch checks client/request identity and pins the client
  and module until the callback returns. Cancel removes queued results/events but
  does not reclaim active provider or callback storage. Reentrant cancel never
  waits for its own callback. Teardown/failed-init rollback must retire clients
  before code or context is freed, suppress queued notifications, and defer reuse
  while work or callbacks remain active. Rollback cannot depend on `fini` being
  called: a module's `init` may have acquired a client before returning failure.
- Keep the old pinned-bank API unchanged. Selecting a listed entry later requires
  source revalidation; a directory cursor is not a file lease or content snapshot.

The pure `files_job::Broker` now tests those lifecycle barriers with owned work
and event leases. Readiness hints coalesce: polling consumes a queued hint for
the observed result, while a notification already in progress keeps its own pin.
Its return cannot erase a newer queued event. Runtime `files.rs` now serializes
short queue operations with the interrupt mask, runs providers and callbacks
outside that scope, and pins active callbacks through return. Loader rollback
and teardown retire clients before freeing module code or context; initialization
must succeed before provider or notification dispatch begins.

## Public contract

Runtime 0.5.4 appends four calls to the existing API table (minor version 1.4),
retaining the pinned-bank entries. Include `<dk/files.h>`:

- `files_client()` obtains the module's client during initialization.
- `files_list(client, root, path, notify, context)` copies a request and returns
  a positive generation ticket. It supersedes the client's prior listing.
- `files_poll(client, ticket, output, capacity)` copies one ready entry to owned
  output, returning pending, entry or end; negative errors are explicit. It does
  not initiate storage I/O. Output remains untouched on pending/end/error.
- `files_cancel(client, ticket)` invalidates that request without waiting for an
  active provider call or callback. Client lifetime remains tied to its module.

An optional notification carries client, ticket, readiness/error hint and caller
context. It is a deferred hint to poll, not a borrowed result or a guaranteed
event for every row. Callback/context must stay in module-owned memory for the
session, as with SDK UI callbacks. Provider and notification dispatch must stay
disabled for a module until its initialization has successfully completed.

The 304-byte entry uses four 32-bit fields for kind, metadata-validity
flags, source byte length and hash algorithm, then 32 hash bytes at offset 16
and a 256-byte full path at offset 48.
Directories need not have size/hash metadata; unknown must not be represented as
known zero. Paths are relative to the captured root. Store SHA-256 stays bytewise;
DT2's odd stored 32-bit hash occupies the first four hash bytes in big-endian
order, with the remaining bytes zero. Never expose a guessed writable/read-only flag: the DT2 ReadDir
flag is currently known only as a boolean derived from two filesystem node bits.
The public C fixture asserts the record layout and the unchanged API prefix.
Both prepared DKMs require API 1.4 and runtime 0.5.4; native acceptance checks
the running table too. This contract does not replace the pinned sample-bank API.

## DN2 provider

The generic store cursor retains a verified header and observed generation.
Each raw page has one bounded table read and before/after header checks. Initial
CRC/tail verification must become stepwise before use from the listing scheduler;
the existing synchronous verified-open call is not a bounded scheduler step.
`CatalogOpen` now implements that stepwise path: the header-capture step performs
at most six single-sector reads; later steps perform four header reads and one
table chunk bounded by scratch capacity and 128 sectors. CRC and tail checking
share each chunk. Alternate captured headers retain the redundant-copy fallback.
Captured header I/O failures deliberately fail closed rather than hiding a
transient failed B-header read as the legacy opener can. `CatalogCursor::rewind`
checks the retained generation before replaying a table for another directory
page; it neither adopts a new generation nor repeats full verification.

`store::directory::DirectoryCollector<N>` forms immediate children from a full
raw scan, including empty explicit and implied directories. It retains the
lexicographically smallest N unique paths above a continuation key; metadata for
files remains exact. No partial directory page is visible before full scan EOF
and generation validation. Empty filtered chunks do not mean EOF. Root exists
even when empty; adapters handle virtual mount roots separately.

This deliberately bounds memory rather than total traversal work. Each page
replays the table: O(table entries * N) insertion work, O(N) retained entries.
Continuations must stay within the same observed generation. Retained-child
conflicts are rejected; this is not a whole-table consistency audit. Before a
performance claim, measure representative catalogue sizes with the repository's
paired measurement tooling. An index/cache can be added behind this interface
later without exposing raw table order to modules.

## DT2 provider

The plain directory opener and iterator have passed a live UI-task proof.
The runtime can retain caller-owned state, copy each full name and obtain stored
metadata under the firmware's own locking. One firmware operation per scheduler
step bounds our call count, not latency: lock contention, record skipping and
storage reads can block. Do not call these routines with interrupts masked or
with an outer filesystem lock.

Raw dot records are omitted by name. Preserve other leading-dot names. Stored
hash and size conversion follow the independently audited getter. A metadata
getter failure retains the file row with validity flags clear; open/iteration
errors terminate the request. Concurrent directory mutation has no proven generation
token or immutable snapshot; the public contract must not promise one.

## Integration checks

Test cancellation/replacement during an active provider operation, stale client
and request handles, end versus pending, callback reentrancy, bounded buffering,
teardown, and interleaved clients. Then validate the actual public C boundary and
both provider adapters in bounded emulator cases before building picker flows.
The existing UI host fake will cover track capture, folder navigation and stale
result rejection without claiming a sample is loaded.

The scheduler performs provider steps, each with its deferred notification, in a
loop per UI pass: it stops when there is no work or event, after about 5 ms (8
firmware blocks), or after 32 steps (8 steps when the firmware counter is absent). Store scratch is 4,096 bytes, with a separate 512-byte fingerprint
buffer, 16 raw rows and eight retained directory children per client. The initial
implementation reserves roughly 42 KiB of fixed ColdFire listing storage for
eight clients. This is a memory/call-count ceiling, not a latency measurement;
DT2 firmware calls can block and each store directory page rescans the table.
