#include "dsp/sample_bank.inc"
// Isolated DN2 1.12 quiet diagnostic. Same byte-address receive/ACK and hook
// save convention as core/dsp-link. DDR stores explicitly use normal-word DM.
// Every payload range and checksum is checked before the first destination store.
.SECTION/PM seg_pmco;
.GLOBAL _dd_hook;
.EXTERN _dd_back;
_dd_hook:
    DM(_dd_sv0) = R0;
    DM(_dd_sv3) = R3;
    DM(_dd_sv4) = R4;
    DM(_dd_sv5) = R5;
    DM(_dd_sv6) = R6;
    R0 = M5;
    DM(_dd_svm5) = R0;
    R0 = M6;
    DM(_dd_svm6) = R0;
    R0 = L2;
    DM(_dd_svl2) = R0;
    R0 = L3;
    DM(_dd_svl3) = R0;
    // Raw context before observer arithmetic; ASTATX low 12 is retained raw.
    R0 = ASTATX;
    DM(_dd_obs_tail_last_astatx) = R0;
    R0 = MODE1;
    DM(_dd_obs_tail_last) = R0;
    R0 = I6;
    DM(_dd_obs_tail_last_i6) = R0;
    R0 = I7;
    DM(_dd_obs_tail_last_i7) = R0;
    R0 = I13;
    DM(_dd_obs_tail_last_i13) = R0;
    R0 = DM(_dd_obs_active);
    DM(_dd_obs_tail_last_active) = R0;
    R0 = DM(_dd_obs_tail_entries);
    R0 = PASS R0;
    IF NE JUMP _dd_obs_tail_seen;
    R0 = DM(_dd_obs_tail_last);
    DM(_dd_obs_tail_first) = R0;
    R0 = DM(_dd_obs_tail_last_astatx);
    DM(_dd_obs_tail_first_astatx) = R0;
    R0 = DM(_dd_obs_tail_last_i6);
    DM(_dd_obs_tail_first_i6) = R0;
    R0 = DM(_dd_obs_tail_last_i7);
    DM(_dd_obs_tail_first_i7) = R0;
    R0 = DM(_dd_obs_tail_last_i13);
    DM(_dd_obs_tail_first_i13) = R0;
    R0 = DM(_dd_obs_tail_last_active);
    DM(_dd_obs_tail_first_active) = R0;
_dd_obs_tail_seen:
    R0 = DM(_dd_obs_tail_entries);
    R0 = R0 + 1;
    DM(_dd_obs_tail_entries) = R0;
    R0 = DM(_dd_obs_active);
    R0 = PASS R0;
    IF EQ JUMP _dd_obs_tail_quiet;
    R0 = DM(_dd_obs_tail_active);
    R0 = R0 + 1;
    DM(_dd_obs_tail_active) = R0;
_dd_obs_tail_quiet:
    M5 = 0;
    M6 = 1;
    L2 = 0;
    L3 = 0;
    R0 = DM(-3, I6);
    I3 = R0;
    DM(_dd_ring) = R0;
    NOP; NOP; NOP;
    R3 = DM(I3, M6);
    R4 = 4;
    COMP(R3, R4);
    IF NE JUMP _dd_normal;
    R3 = DM(I3, M6);
    R4 = 0xDDF4A601;
    COMP(R3, R4);
    IF NE JUMP _dd_ack;
    R3 = DM(_dd_status);
    R4 = 2;
    COMP(R3, R4);
    IF LT JUMP _dd_header;
    R4 = 4;
    COMP(R3, R4);
    IF LE JUMP _dd_ack; // only errors 2..4 are sticky poison
_dd_header:
    R10 = DM(I3, M6); // exact token
    R11 = DM(I3, M6); // sequence
    R13 = DM(I3, M6); // operation
    R14 = DM(I3, M6); // destination offset in words
    R15 = DM(I3, M6); // count in words
    R0 = DM(I3, M6);
    DM(_dd_check) = R0;
    R3 = DM(_dd_token);
    R3 = PASS R3;
    IF EQ JUMP _dd_initial;
    COMP(R10, R3);
    IF EQ JUMP _dd_bounds;
    // Only the exact successor INIT may replace a fully retired generation.
    R4 = -1;
    COMP(R3, R4);
    IF EQ JUMP _dd_ack; // boot-scoped token exhaustion, never wrap or reuse
    R3 = R3 + 1;
    COMP(R10, R3);
    IF NE JUMP _dd_ack;
    R11 = PASS R11;
    IF NE JUMP _dd_ack;
    R3 = 1;
    COMP(R13, R3);
    IF NE JUMP _dd_ack;
    R3 = R14 OR R15;
    IF NE JUMP _dd_ack;
    R3 = DM(_dd_status);
    R4 = 5;
    COMP(R3, R4);
    IF NE JUMP _dd_ack;
    R3 = DM(_dd_admitted);
    R4 = DM(_dd_admit_fault);
    R3 = R3 OR R4;
    IF NE JUMP _dd_ack;
    JUMP _dd_bounds;
_dd_initial:
    R10 = PASS R10;
    IF EQ JUMP _dd_protocol;
    R11 = PASS R11;
    IF NE JUMP _dd_protocol;
    R3 = 1;
    COMP(R13, R3);
    IF NE JUMP _dd_protocol;
    R3 = R14 OR R15;
    IF NE JUMP _dd_protocol;
_dd_bounds:
    R15 = PASS R15;
    IF LT JUMP _dd_range;
    R3 = 640;
    COMP(R15, R3);
    IF GT JUMP _dd_range;
    R14 = PASS R14;
    IF LT JUMP _dd_range;
    R3 = 240000;
    COMP(R14, R3);
    IF GT JUMP _dd_range;
    R3 = R3 - R14;
    COMP(R15, R3);
    IF GT JUMP _dd_range;
    // Check header words 0..6, then exactly count payload words.
    R0 = DM(_dd_ring);
    I3 = R0;
    R3 = 0;
    R4 = 0;
    NOP; NOP;
    LCNTR = 7, DO _dd_hsum UNTIL LCE;
        R0 = DM(I3, M6);
        R3 = R0 + R3;
        NOP;
_dd_hsum: R4 = R3 + R4;
    R0 = DM(_dd_ring);
    R5 = 128;
    R0 = R0 + R5;
    I3 = R0;
    DM(_dd_payload) = R0;
    R15 = PASS R15;
    IF EQ JUMP _dd_sumdone;
    NOP;
    LCNTR = R15, DO _dd_psum UNTIL LCE;
        R0 = DM(I3, M6);
        R3 = R0 + R3;
        NOP;
_dd_psum: R4 = R3 + R4;
_dd_sumdone:
    R3 = R3 XOR R4;
    R4 = DM(_dd_check);
    COMP(R3, R4);
    IF NE JUMP _dd_badcheck;
    R3 = DM(_dd_token);
    R3 = PASS R3;
    IF EQ JUMP _dd_init;
    COMP(R10, R3);
    IF NE JUMP _dd_successor;
    R3 = 4;
    COMP(R13, R3);
    IF EQ JUMP _dd_retire;
    R3 = DM(_dd_retire_closed);
    R3 = PASS R3;
    IF NE JUMP _dd_ack; // this generation cannot reopen after retirement
    R3 = 5;
    COMP(R13, R3);
    IF EQ JUMP _dd_open;
    R3 = 1;
    COMP(R13, R3);
    IF EQ JUMP _dd_ack; // replayed INIT never resets a generation or ACK identity
    // Only an immediate exact duplicate of loading DATA/COMMIT is idempotent.
    R3 = DM(_dd_seq);
    COMP(R11, R3);
    IF NE JUMP _dd_next;
    R3 = DM(_dd_lastcheck);
    COMP(R3, R4);
    IF NE JUMP _dd_protocol;
    JUMP _dd_ack;
_dd_next:
    R3 = DM(_dd_status);
    R3 = PASS R3;
    IF NE JUMP _dd_ack; // a sealed or open bank accepts no further payload
    R3 = 3;
    COMP(R13, R3);
    IF EQ JUMP _dd_commit;
    R3 = 2;
    COMP(R13, R3);
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_seq);
    R3 = R3 + 1;
    COMP(R11, R3);
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_words);
    COMP(R14, R3);
    IF NE JUMP _dd_range;
    R15 = PASS R15;
    IF EQ JUMP _dd_range;
    R3 = 375;
    COMP(R11, R3);
    IF GT JUMP _dd_protocol;
    R3 = 0x10200000;
    R0 = R3 + R14;
    I2 = R0;
    R0 = DM(_dd_payload);
    I3 = R0;
    NOP; NOP; NOP; NOP;
    LCNTR = R15, DO _dd_copy UNTIL LCE;
        R0 = DM(I3, M6);
        NOP; NOP;
_dd_copy: DM(I2, M6) = R0 (NW);
    R3 = DM(_dd_words);
    R3 = R3 + R15;
    DM(_dd_words) = R3;
    JUMP _dd_accepted;
_dd_commit:
    R3 = 376;
    COMP(R11, R3);
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_seq);
    R4 = 375;
    COMP(R3, R4);
    IF NE JUMP _dd_protocol;
    R15 = PASS R15;
    IF NE JUMP _dd_range;
    R3 = 240000;
    COMP(R14, R3);
    IF NE JUMP _dd_range;
    R4 = DM(_dd_words);
    COMP(R3, R4);
    IF NE JUMP _dd_range;
    R3 = 1;
    DM(_dd_status) = R3; // SEALED, not READY: only explicit OPEN admits notes
    JUMP _dd_accepted;
_dd_successor:
    // Header, checksum and exact successor were checked before any mutation.
    // Recheck the drain under the same local gate used by readers and retirement.
    DK_SAMPLE_BANK_LOCK(R6);
    R3 = DM(_dd_status);
    R4 = 5;
    COMP(R3, R4);
    IF NE JUMP _dd_successor_denied;
    DK_SAMPLE_BANK_CHECK_DRAIN(_dd_admitted, _dd_admit_fault, R3, R4, _dd_successor_denied);
    DK_SAMPLE_BANK_UNLOCK(R6);
    JUMP _dd_init;
_dd_successor_denied:
    DK_SAMPLE_BANK_UNLOCK(R6);
    JUMP _dd_ack;
_dd_init:
    // Admission is closed and the previous generation drained before either band.
    // The successor token is published only after both complete guard bands.
    I2 = 0x101ffc00;
    R0 = 0xA55A3CC3;
    NOP; NOP; NOP; NOP;
    LCNTR = 1024, DO _dd_lower UNTIL LCE;
        NOP; NOP; NOP;
_dd_lower: DM(I2, M6) = R0 (NW);
    I2 = 0x1023a980;
    NOP; NOP; NOP; NOP;
    LCNTR = 1024, DO _dd_upper UNTIL LCE;
        NOP; NOP; NOP;
_dd_upper: DM(I2, M6) = R0 (NW);
    R3 = 0;
    DM(_dd_ready) = R3;
    DM(_dd_status) = R3;
    DM(_dd_words) = R3;
    DM(_dd_admission_open) = R3;
    DM(_dd_retire_closed) = R3;
    DM(_dd_token) = R10;
_dd_accepted:
    DM(_dd_seq) = R11;
    DM(_dd_ackseq) = R11;
    R3 = DM(_dd_check);
    DM(_dd_lastcheck) = R3;
    JUMP _dd_ack;
_dd_open:
    R3 = 378;
    COMP(R11, R3);
    IF NE JUMP _dd_protocol;
    R3 = R14 OR R15;
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_status);
    R4 = 7;
    COMP(R3, R4);
    IF EQ JUMP _dd_ack; // duplicate OPEN only while this generation remains open
    R4 = 1;
    COMP(R3, R4);
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_seq);
    R4 = 376;
    COMP(R3, R4);
    IF NE JUMP _dd_protocol;
    DK_SAMPLE_BANK_LOCK(R6);
    DK_SAMPLE_BANK_CHECK_DRAIN(_dd_admitted, _dd_admit_fault, R3, R4, _dd_open_fault);
    DK_SAMPLE_BANK_PUBLISH(_dd_admission_open, _dd_ready, R3);
    R3 = 7;
    DM(_dd_status) = R3;
    DK_SAMPLE_BANK_UNLOCK(R6);
    JUMP _dd_accepted;
_dd_open_fault:
    DK_SAMPLE_BANK_UNLOCK(R6);
    JUMP _dd_protocol;
// RETIRE uses the exact completed bank token and fixed terminal sequence.
// Serialized hook + one core + maskable access are proof assumptions; this
// local mask does not establish universal firmware exclusion or NMI safety.
_dd_retire:
    R3 = 377;
    COMP(R11, R3);
    IF NE JUMP _dd_protocol;
    R3 = R14 OR R15;
    IF NE JUMP _dd_protocol;
    R3 = DM(_dd_seq);
    R4 = 376;
    COMP(R3, R4);
    IF LT JUMP _dd_protocol;
    R3 = DM(_dd_status);
    R4 = 1;
    COMP(R3, R4);
    IF LT JUMP _dd_protocol;
    DK_SAMPLE_BANK_LOCK(R6);
    DK_SAMPLE_BANK_CLOSE(_dd_admission_open, _dd_ready, _dd_retire_closed, R3);
    DK_SAMPLE_BANK_CHECK_DRAIN(_dd_admitted, _dd_admit_fault, R3, R4, _dd_retire_pending);
    R3 = 5;
    JUMP _dd_retire_status;
_dd_retire_pending:
    R3 = 6;
_dd_retire_status:
    DM(_dd_status) = R3;
    DK_SAMPLE_BANK_UNLOCK(R6);
    JUMP _dd_accepted;
_dd_badcheck:
    R3 = DM(_dd_token);
    R3 = PASS R3;
    IF EQ JUMP _dd_badcheck_current;
    COMP(R10, R3);
    IF NE JUMP _dd_ack;
_dd_badcheck_current:
    R3 = 2;
    JUMP _dd_error;
_dd_range:
    R3 = 3;
    JUMP _dd_error;
_dd_protocol:
    R3 = 4;
_dd_error:
    DM(_dd_status) = R3;
    R3 = 0;
    DM(_dd_ready) = R3;
    DM(_dd_admission_open) = R3;
    JUMP _dd_ack;
_dd_normal:
    // The stock dispatch reads unsigned SHORT16; the adjacent high half holds
    // pitch after note-on. Only this observer masks it. The proof gate above
    // still requires the exact complete word 4 and separate proof magic.
    R3 = FEXT R3 BY 0:16;
    R4 = 3;
    COMP(R3, R4);
    IF NE JUMP _dd_ack;
    R3 = DM(_dd_normal_frames);
    R3 = R3 + 1;
    DM(_dd_normal_frames) = R3;
_dd_ack:
    R3 = DM(_dd_token);
    R3 = PASS R3;
    IF EQ JUMP _dd_out;
    R4 = I4;
    R5 = 0x60;
    R4 = R4 + R5;
    I2 = R4;
    R4 = DM(_dd_ackseq);
    R4 = LSHIFT R4 BY 16;
    R5 = 0xA6DD;
    R4 = R4 OR R5;
    R5 = DM(_dd_status);
    R5 = LSHIFT R5 BY 25;
    R4 = R4 OR R5;
    DM(I2, M6) = R4;
    DM(I2, M6) = R3;
_dd_out:
    R0 = DM(_dd_obs_tail_exits);
    R0 = R0 + 1;
    DM(_dd_obs_tail_exits) = R0;
    R0 = DM(_dd_svm5);
    M5 = R0;
    R0 = DM(_dd_svm6);
    M6 = R0;
    R0 = DM(_dd_svl2);
    L2 = R0;
    R0 = DM(_dd_svl3);
    L3 = R0;
    R0 = DM(_dd_sv0);
    R3 = DM(_dd_sv3);
    R4 = DM(_dd_sv4);
    R5 = DM(_dd_sv5);
    R6 = DM(_dd_sv6);
    JUMP _dd_back;

.SECTION/DM seg_dmda;
.GLOBAL _dd_token;
.VAR _dd_token = 0;
.GLOBAL _dd_seq;
.VAR _dd_seq = 0;
.GLOBAL _dd_ackseq;
.VAR _dd_ackseq = 0;
.GLOBAL _dd_status;
.VAR _dd_status = 0;
.GLOBAL _dd_words;
.VAR _dd_words = 0;
.GLOBAL _dd_ready;
.VAR _dd_ready = 0;
.GLOBAL _dd_check;
.VAR _dd_check = 0;
.GLOBAL _dd_lastcheck;
.VAR _dd_lastcheck = 0;
.GLOBAL _dd_ring;
.VAR _dd_ring = 0;
.GLOBAL _dd_payload;
.VAR _dd_payload = 0;
.GLOBAL _dd_sv0;
.VAR _dd_sv0 = 0;
.GLOBAL _dd_sv3;
.VAR _dd_sv3 = 0;
.GLOBAL _dd_sv4;
.VAR _dd_sv4 = 0;
.GLOBAL _dd_sv5;
.VAR _dd_sv5 = 0;
.GLOBAL _dd_sv6;
.VAR _dd_sv6 = 0;
.GLOBAL _dd_svm5;
.VAR _dd_svm5 = 0;
.GLOBAL _dd_svm6;
.VAR _dd_svm6 = 0;
.GLOBAL _dd_svl2;
.VAR _dd_svl2 = 0;
.GLOBAL _dd_svl3;
.VAR _dd_svl3 = 0;

.GLOBAL _dd_normal_frames;
.VAR _dd_normal_frames = 0;

// Fixed storage only: no trace buffer and no retirement/admission semantics.
// The active marker is a non-atomic read/modify/write observation. Missed
// preemption, another bank/core, or counter wrap can hide nesting. A zero
// overlap count is not proof of exclusion. First/latest snapshots can tear
// when inspected live; ASTATX is not a task identity.
.SECTION/DM seg_dmda;
.GLOBAL _dd_obs_active;
.VAR _dd_obs_active = 0;
.GLOBAL _dd_obs_tail_active;
.VAR _dd_obs_tail_active = 0;
.GLOBAL _dd_obs_note_entries;
.VAR _dd_obs_note_entries = 0;
.GLOBAL _dd_obs_note_exits;
.VAR _dd_obs_note_exits = 0;
.GLOBAL _dd_obs_note_first;
.VAR _dd_obs_note_first = 0;
.VAR _dd_obs_note_first_astatx = 0;
.VAR _dd_obs_note_first_i6 = 0;
.VAR _dd_obs_note_first_i7 = 0;
.VAR _dd_obs_note_first_i13 = 0;
.VAR _dd_obs_note_first_active = 0;
.GLOBAL _dd_obs_note_last;
.VAR _dd_obs_note_last = 0;
.VAR _dd_obs_note_last_astatx = 0;
.VAR _dd_obs_note_last_i6 = 0;
.VAR _dd_obs_note_last_i7 = 0;
.VAR _dd_obs_note_last_i13 = 0;
.VAR _dd_obs_note_last_active = 0;
.GLOBAL _dd_obs_render_entries;
.VAR _dd_obs_render_entries = 0;
.GLOBAL _dd_obs_render_exits;
.VAR _dd_obs_render_exits = 0;
.GLOBAL _dd_obs_render_first;
.VAR _dd_obs_render_first = 0;
.VAR _dd_obs_render_first_astatx = 0;
.VAR _dd_obs_render_first_i6 = 0;
.VAR _dd_obs_render_first_i7 = 0;
.VAR _dd_obs_render_first_i13 = 0;
.VAR _dd_obs_render_first_active = 0;
.GLOBAL _dd_obs_render_last;
.VAR _dd_obs_render_last = 0;
.VAR _dd_obs_render_last_astatx = 0;
.VAR _dd_obs_render_last_i6 = 0;
.VAR _dd_obs_render_last_i7 = 0;
.VAR _dd_obs_render_last_i13 = 0;
.VAR _dd_obs_render_last_active = 0;
.GLOBAL _dd_obs_tail_entries;
.VAR _dd_obs_tail_entries = 0;
.GLOBAL _dd_obs_tail_exits;
.VAR _dd_obs_tail_exits = 0;
.GLOBAL _dd_obs_tail_first;
.VAR _dd_obs_tail_first = 0;
.VAR _dd_obs_tail_first_astatx = 0;
.VAR _dd_obs_tail_first_i6 = 0;
.VAR _dd_obs_tail_first_i7 = 0;
.VAR _dd_obs_tail_first_i13 = 0;
.VAR _dd_obs_tail_first_active = 0;
.GLOBAL _dd_obs_tail_last;
.VAR _dd_obs_tail_last = 0;
.VAR _dd_obs_tail_last_astatx = 0;
.VAR _dd_obs_tail_last_i6 = 0;
.VAR _dd_obs_tail_last_i7 = 0;
.VAR _dd_obs_tail_last_i13 = 0;
.VAR _dd_obs_tail_last_active = 0;

.GLOBAL _dd_admission_open;
.VAR _dd_admission_open = 0;

.GLOBAL _dd_admitted;
.VAR _dd_admitted = 0;

.GLOBAL _dd_retire_closed;
.VAR _dd_retire_closed = 0;

.GLOBAL _dd_admit_fault;
.VAR _dd_admit_fault = 0;
