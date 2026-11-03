// Per-track admission and epoch operations for the track sample family (<dk/sample_track.h>, API 1.5).
// The same sequences as sample_bank.inc, with the words of one track addressed through two caller I registers instead of
// fixed symbols (one set of macros serves all 16 tracks):
//   REC = the track's dk_strack record, normal-word address: word 0 ready (the OPEN and READY word of the bank macros), word 1 token.
//   CTL = the track's control set: word 0 closed, word 1 count, word 2 fault (DK_STRACK_CTL_*).
// Include dsp/sample_bank.inc first for DK_SAMPLE_BANK_LOCK / UNLOCK; every rule of that file holds here (interrupt mask held for all
// operations, CHECK_ADMISSION before CHECK_EPOCH/LATCH_EPOCH and ACQUIRE in one scope, RELEASE after the last read of the PCM, FAIL_CLOSED then
// POISON_COUNT on a RELEASE fault). A DAG register that was just loaded needs three instructions before it is used (the callers' NOPs).
// REC and CTL must differ and must not be clobbered in the masked region. Instruction counts as the bank macros.
#ifndef DK_SAMPLE_TRACK_INC
#define DK_SAMPLE_TRACK_INC

#define DK_STRACK_CHECK_ADMISSION(REC, CTL, TMP, LIMIT, DENIED, FAULT) \
    TMP = DM(0, REC); \
    TMP = PASS TMP; \
    IF EQ JUMP DENIED; \
    TMP = DM(0, CTL); \
    TMP = PASS TMP; \
    IF NE JUMP DENIED; \
    TMP = DM(1, CTL); \
    TMP = PASS TMP; \
    IF LT JUMP FAULT; \
    LIMIT = 0x7fffffff; \
    COMP(TMP, LIMIT); \
    IF GE JUMP FAULT

#define DK_STRACK_CHECK_EPOCH(REC, STATE, OFFSET, CACHED, CURRENT, DENIED) \
    CACHED = DM(OFFSET, STATE); \
    CACHED = PASS CACHED; \
    IF EQ JUMP DENIED; \
    CURRENT = DM(1, REC); \
    COMP(CACHED, CURRENT); \
    IF NE JUMP DENIED

#define DK_STRACK_LATCH_EPOCH(REC, STATE, OFFSET, EPOCH, ONE, DENIED) \
    EPOCH = DM(0, REC); \
    ONE = 1; \
    COMP(EPOCH, ONE); \
    IF NE JUMP DENIED; \
    EPOCH = DM(1, REC); \
    EPOCH = PASS EPOCH; \
    IF EQ JUMP DENIED; \
    DM(OFFSET, STATE) = EPOCH

#define DK_STRACK_ACQUIRE(CTL, TMP) \
    TMP = DM(1, CTL); \
    TMP = TMP + 1; \
    DM(1, CTL) = TMP

#define DK_STRACK_FAIL_CLOSED(REC, CTL, TMP) \
    TMP = 1; \
    DM(2, CTL) = TMP; \
    DM(0, CTL) = TMP; \
    TMP = 0; \
    DM(0, REC) = TMP

#define DK_STRACK_RELEASE(CTL, TMP, FAULT, DONE) \
    TMP = DM(1, CTL); \
    TMP = PASS TMP; \
    IF LE JUMP FAULT; \
    TMP = TMP - 1; \
    DM(1, CTL) = TMP; \
    JUMP DONE

#define DK_STRACK_POISON_COUNT(CTL, TMP) \
    TMP = 0x7fffffff; \
    DM(1, CTL) = TMP

#endif
