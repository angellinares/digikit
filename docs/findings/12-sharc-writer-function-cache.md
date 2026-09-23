# Per-function SHARC writer-fact cache

**[D]** The SHARC analysis index v5 persists target-independent writer trace
facts one recovered function at a time.  Each row is bound to the static index
key, recovered function ID, entry, ordinal, and a canonical DM-store shape.
The payload is canonical JSON with a digest/size and a completion marker; it
records stores/events, stop reasons, provisional path forms, and conservative
form/blocker dependencies.  Rows with malformed identity, digest, size, or
completion state are cache misses.

**[V]** Malformed per-function payload or dependency metadata is treated as a
cache miss.

**[D]** A trace-core revision is part of the function-fact request and thus
invalidates all writer facts.  A versioned provisional-form policy is evaluated
from each fact's recorded forms and blockers, so a changed admission policy
reuses facts with no dependency on its changed forms and retraces dependent
facts.  Unknown policy/dependency metadata fails closed.  This is cache
behaviour only.

**[V]** This bounded phase changes only cache/index behavior and does not
promote any firmware finding.

**[D]** Aggregation remains deterministic: facts are ordered by stable recovered
function ID/ordinal before existing target classification.  Publication and
corruption repair occur in one parent SQLite transaction.  Existing v4 caches
are incompatible with the v5 public contract and are rebuilt rather than
partially reused.

**[V]** Existing findings 10 and 11 remain documentary/open and are not newly
marked **[V]**.
