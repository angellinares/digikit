// Sample-bank lifecycle operations: caller-owned state, layout and control flow.
// Include once or repeatedly; no storage, labels or externs are declared.
// Control words and the cached epoch slot must not overlap one another or the
// backing/output. R/F aliases count as the same register.
// All R/I operands name caller registers; use distinct register operands within
// each macro, and keep LOCK's SAVED live and distinct from every register
// mutated in the masked region until UNLOCK. Do not switch register banks in
// that region: SAVED must remain readable in the incoming selected bank.
// All CHECK/update operations require the caller's local interrupt mask held.
// CHECK_ADMISSION assumes sticky FAULT implies CLOSED != 0, OPEN/READY == 0;
// it does not read FAULT. It precedes epoch validation and ACQUIRE in one scope;
// ACQUIRE requires a validated count in [0, 0x7ffffffe]. RELEASE follows the last
// possible backing read. Its FAULT target must FAIL_CLOSED and POISON_COUNT:
// corrupt bookkeeping must never establish a completed drain or permit reuse.
// CHECK_EPOCH/LATCH_EPOCH require an initialized STATE I register (DAG latency).
// LATCH_EPOCH is only for fresh voices; cached voices use CHECK_EPOCH.
// CLOSE precedes CHECK_DRAIN; only a successful exact drain permits reuse.
// PUBLISH requires an immutable, fully initialized generation and a successful
// drain with no fault, under a serialized producer's ownership. The producer
// validates a nonzero successor token; never reopen a retired generation. It does not
// clear CLOSED: caller initializes CLOSED = 0 only for that validated successor
// before publication, with FAULT = 0 and nonzero TOKEN.
// State operands are normal DM address symbols; OFFSET is a caller state offset.
// No allocation, token succession, wire protocol or backing layout is supplied.
//
// LOCK/UNLOCK: 4/3 instructions; exact incoming MODE1 saved/restored, IRPTEN
// (0x1000) cleared with two conservative NOPs after each mode change. No flags.
// Other operations clobber only their named R temporaries and arithmetic flags;
// CHECK_EPOCH does not modify STATE. Memory writes are exactly those named.
// Instructions: CHECK_ADMISSION 12, CHECK_EPOCH 6, LATCH_EPOCH 8, ACQUIRE 3,
// FAIL_CLOSED 6, RELEASE 6, POISON_COUNT 2, CLOSE 5, CHECK_DRAIN 4, PUBLISH 3.
// Single core, maskable participants and one serialized producer are required;
// masking does not establish NMI exclusion, hardware ownership or cache safety.
// The caller supplies every branch target and the trailing semicolon.
#ifndef DK_SAMPLE_BANK_INC
#define DK_SAMPLE_BANK_INC

#define DK_SAMPLE_BANK_LOCK(SAVED) \
    SAVED = MODE1; \
    BIT CLR MODE1 0x1000; \
    NOP; \
    NOP

#define DK_SAMPLE_BANK_UNLOCK(SAVED) \
    MODE1 = SAVED; \
    NOP; \
    NOP

#define DK_SAMPLE_BANK_CHECK_ADMISSION(OPEN, CLOSED, COUNT, TMP, LIMIT, DENIED, FAULT) \
    TMP = DM(OPEN); \
    TMP = PASS TMP; \
    IF EQ JUMP DENIED; \
    TMP = DM(CLOSED); \
    TMP = PASS TMP; \
    IF NE JUMP DENIED; \
    TMP = DM(COUNT); \
    TMP = PASS TMP; \
    IF LT JUMP FAULT; \
    LIMIT = 0x7fffffff; \
    COMP(TMP, LIMIT); \
    IF GE JUMP FAULT

#define DK_SAMPLE_BANK_CHECK_EPOCH(TOKEN, STATE, OFFSET, CACHED, CURRENT, DENIED) \
    CACHED = DM(OFFSET, STATE); \
    CACHED = PASS CACHED; \
    IF EQ JUMP DENIED; \
    CURRENT = DM(TOKEN); \
    COMP(CACHED, CURRENT); \
    IF NE JUMP DENIED

#define DK_SAMPLE_BANK_LATCH_EPOCH(READY, TOKEN, STATE, OFFSET, EPOCH, ONE, DENIED) \
    EPOCH = DM(READY); \
    ONE = 1; \
    COMP(EPOCH, ONE); \
    IF NE JUMP DENIED; \
    EPOCH = DM(TOKEN); \
    EPOCH = PASS EPOCH; \
    IF EQ JUMP DENIED; \
    DM(OFFSET, STATE) = EPOCH

#define DK_SAMPLE_BANK_ACQUIRE(COUNT, TMP) \
    TMP = DM(COUNT); \
    TMP = TMP + 1; \
    DM(COUNT) = TMP

#define DK_SAMPLE_BANK_FAIL_CLOSED(FAULT, CLOSED, OPEN, READY, TMP) \
    TMP = 1; \
    DM(FAULT) = TMP; \
    DM(CLOSED) = TMP; \
    TMP = 0; \
    DM(OPEN) = TMP; \
    DM(READY) = TMP

#define DK_SAMPLE_BANK_RELEASE(COUNT, TMP, FAULT, DONE) \
    TMP = DM(COUNT); \
    TMP = PASS TMP; \
    IF LE JUMP FAULT; \
    TMP = TMP - 1; \
    DM(COUNT) = TMP; \
    JUMP DONE

#define DK_SAMPLE_BANK_POISON_COUNT(COUNT, TMP) \
    TMP = 0x7fffffff; \
    DM(COUNT) = TMP

#define DK_SAMPLE_BANK_CLOSE(OPEN, READY, CLOSED, TMP) \
    TMP = 0; \
    DM(OPEN) = TMP; \
    DM(READY) = TMP; \
    TMP = 1; \
    DM(CLOSED) = TMP

#define DK_SAMPLE_BANK_CHECK_DRAIN(COUNT, FAULT, TMP, OTHER, NONEMPTY) \
    TMP = DM(COUNT); \
    OTHER = DM(FAULT); \
    TMP = TMP OR OTHER; \
    IF NE JUMP NONEMPTY

#define DK_SAMPLE_BANK_PUBLISH(OPEN, READY, TMP) \
    TMP = 1; \
    DM(OPEN) = TMP; \
    DM(READY) = TMP

#endif
