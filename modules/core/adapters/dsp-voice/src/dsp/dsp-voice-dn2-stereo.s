// core/dsp-voice on the Digitone II: stereo machines (machine contract 1.2), docs/design/dn2-stereo.md.
//
// A machine that returns DK_STEREO writes R to a per-slot R buffer (`_gs_rbuf`). `_gs_post` (called by the contract adapter after the guard) checks and
// scales R, and stamps the slot "stereo in this block": `_gs_stamp[v] = heartbeat` (`_dv_diag` word 0). The stamp expires with the next block. The
// firmware chain that follows the machine (docs/findings/96-stereo-machine-processing.md) then runs R through the same stages as L:
//
//   * a twin of a stage runs the stock function a second time on R: a private R state record, the R buffer, and the stage's shared control words copied
//     from the L record BEFORE the L call (the L call advances them once per block). The L call and the L record stay the stock path. Every stage between the
//     machine and the pan has a twin; the twins are retargets of the call sites (`_gs_fo`, `_gs_uy`, `_gs_gain`, `_gs_c5`, `_gs_shaper_*`, `_gs_held_*`,
//     `_gs_case1` to `_gs_case3`, `_gs_ring4`, `_gs_ring6`), and the resets in the middle of a note are mirrored (`_gs_*_reset`).
//   * the twins are also the guard: a context they cannot run in (L3, L4 or L5 nonzero, a block count other than 32) clears the stamp, so the slot plays R = L,
//     and is counted per stage (`_gs_cnt`) and raised in `_gs_health`. So are a bad R and the cap of stereo slots, in `_gs_post`.
//   * the pan loop of the engine (the hook `_gs_slot` inside the gain loop) feeds plane B from R for a stamped slot; the stock instructions run for every other slot.
//
// Slot of a call: the voice buffer R8 is entry v of the table of voice buffers (frame word -24 of the engine's frame); the twins take it from there.
// Registers of a retargeted call (the C calling convention of sdk/dsp/call.inc): R0, R1, R2, I4, I12 and the flags are scratch for a wrapper; R4, R8, R12
// and the stack arguments reach the stock function unchanged. The wrappers need M5 = 0, M6 = 1, M7 = -1, M14 = 1 as the engine holds them.
//
// R0-R2 are read nowhere by the stock callees as arguments (they are caller-saved in the convention); a wrapper does not restore the caller-saved registers
// after a stereo call.
#include "dsp/call.inc"
#include "adapter-dn2.inc"
#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.EXTERN _gs_fo_stock;
.EXTERN _gs_fo_reset_stock;
.EXTERN _gs_uy_stock;
.EXTERN _gs_uy_reset_stock;
.EXTERN _gs_gain_stock;
.EXTERN _gs_c5_stock;
.EXTERN _gs_held_reset_stock;
.EXTERN _gs_case5_reset_stock;
.EXTERN _gs_ring_reset_stock;
.EXTERN _gs_case3_reset_stock;
.EXTERN _gs_case2_reset_stock;
.EXTERN _gs_case1_reset_stock;
.EXTERN _gs_held_stock;
.EXTERN _gs_shaper_stock;
.EXTERN _gs_case1_stock;
.EXTERN _gs_case2_stock;
.EXTERN _gs_case3_stock;
.EXTERN _gs_ring_stock;
.EXTERN _gs_engine_base;
.EXTERN _gs_pan_cont;
.EXTERN _gs_pan_end;
.EXTERN _gs_pre_cont;
.EXTERN _dv_diag;
.EXTERN _dv_c;
.EXTERN _dv_in;

// ---- the slot of a retargeted call: R0 = 4 v. Needs R2 = the caller's frame (the linked call sets it), R8 = the voice buffer. Clobbers R1, I4. ----
#define GS_SLOT4 \
    I4 = R2; \
    R0 = DM(-24, I4); \
    I4 = R0; \
    R0 = DM(0, I4); \
    R0 = R8 - R0; \
    R0 = LSHIFT R0 BY -5

// ---- the stamp check of a twin: falls to the stock function when the slot is not stamped in this block. I4 = the slot's stamp word on return. ----
#define GS_CHECK(STOCK) \
    GS_SLOT4; \
    R1 = _gs_stamp; \
    R0 = R0 + R1; \
    I4 = R0; \
    R1 = DM(0, I4); \
    R0 = DM(_dv_diag); \
    COMP(R0, R1); \
    IF NE JUMP STOCK

// ---- the stereo path needs the linear DAG context (L3 to L5 zero) and the 32-sample block of the stock call; else the slot plays R = L (stamp cleared) and the stock
// function runs. MODE1 PEYEN is clear at every call site: the engine's SIMD loops (0x1c9484 to 0x1c94b0, 0x1c98f9 to 0x1c9926, 0x1c9948 to 0x1c9975,
// 0x1c9aaf to 0x1c9ad2) hold no call, and the stock stages are scalar functions. ----
#define GS_SUPPORTED(BAD, COUNT) \
    R1 = L3; \
    R0 = L4; \
    R1 = R1 OR R0; \
    R0 = L5; \
    R1 = R1 OR R0; \
    R1 = PASS R1; \
    IF NE JUMP BAD; \
    R0 = DM(COUNT, I6); \
    R1 = 32; \
    COMP(R0, R1); \
    IF NE JUMP BAD

// ---- the guard: the refusal of an unsupported context by a twin (L3, L4 or L5 nonzero, block count not 32): clears the stamp (the slot plays R = L), counts it in CNT, in `_gs_nbad`
// and raises bits 0 and 1 of `_gs_hacc`. In: I4 = the slot's stamp word. Clobbers R1, R2. ----
#define GS_REFUSE(CNT) \
    R1 = 0; \
    DM(0, I4) = R1; \
    R1 = DM(CNT); \
    R1 = R1 + 1; \
    DM(CNT) = R1; \
    R1 = DM(_gs_nbad); \
    R1 = R1 + 1; \
    DM(_gs_nbad) = R1; \
    R1 = DM(_gs_hacc); \
    R2 = 3; \
    R1 = R1 OR R2; \
    DM(_gs_hacc) = R1

// A stereo twin saves I0 (callee-saved) in frame slot 4 and uses I0 and I4 as the copy pointers (a DM access takes a DAG1 register: I0 to I7).
// Frame: -2 the R record, -3 the R buffer, -4 I0, -5 I1, -6 I5, -7 R3, -8 R6 (case 5), -9 the R scratch (case 5).
#define GS_SAVE_I0 \
    R0 = I0; \
    DM(-4, I6) = R0

// ---- the common parts of the twins of the families with one stack argument (the block count) and the voice buffer as input and output ----
// GS_TWIN_HEAD: stamp check, supported context, frame, the R record (WORDS words per slot at REC) in frame -2, the R buffer in frame -3, I4 = the L record,
// I0 = the R record. In: R0 free. After it GS_COPY(n) copies a shared control word L to R, then GS_L1 and GS_R1 run the stock stage on L and on R.
#define GS_RPTRS(WORDS, REC) \
    R1 = WORDS; \
    R1 = R1 * R0 (SSI); \
    R2 = REC; \
    R1 = R1 + R2; \
    DM(-2, I6) = R1; \
    R1 = LSHIFT R0 BY 5; \
    R2 = _gs_rbuf; \
    R1 = R1 + R2; \
    DM(-3, I6) = R1

#define GS_TWIN_HEAD(STOCK, REFUSE, WORDS, REC) \
    GS_CHECK(STOCK); \
    GS_SUPPORTED(REFUSE, 1); \
    DK_ENTER(8); \
    GS_SAVE_I0; \
    R0 = I4; \
    R1 = _gs_stamp; \
    R0 = R0 - R1; \
    GS_RPTRS(WORDS, REC); \
    I4 = R4; \
    I0 = DM(-2, I6)

#define GS_COPY(N) \
    R0 = DM(N, I4); \
    DM(N, I0) = R0

#define GS_L1(STOCK, LABEL) \
    R0 = DM(1, I6); \
    DM(I7, M7) = R0; \
    DK_CALL(STOCK, LABEL); \
    MODIFY(I7, 1) (NW)

#define GS_R1(STOCK, LABEL) \
    R4 = DM(-2, I6); \
    R8 = DM(-3, I6); \
    R12 = R8; \
    R0 = DM(1, I6); \
    DM(I7, M7) = R0; \
    DK_CALL(STOCK, LABEL); \
    MODIFY(I7, 1) (NW)

// R buffer times FACTOR in place (the shaper's late path: the engine scales L by 16 before the stage and by 1/16 after it). Clobbers R0, R2, I0, I4.
#define GS_SCALE_R(FACTOR, LABEL) \
    I4 = DM(-3, I6); \
    I0 = DM(-3, I6); \
    R2 = FACTOR; \
    LCNTR = 32, DO LABEL UNTIL LCE; \
        R0 = DM(I4, M6); \
        F0 = F0 * F2; \
LABEL: DM(I0, M6) = R0

// ---- first-order recurrence (0xb80c39, call 0x1c94fd): L record 28 words at engine + 0xe088 + 0x70 v; control words 21 to 23 ----
.GLOBAL _gs_fo;
_gs_fo:
    GS_CHECK(_gs_fo_stock);
    GS_SUPPORTED(_gs_fo_bad, 1);
    DK_ENTER(8);
    GS_SAVE_I0;
    R0 = I4;
    R1 = _gs_stamp;
    R0 = R0 - R1;                         // 4 v
    R1 = LSHIFT R0 BY 5;
    R2 = LSHIFT R0 BY 2;
    R1 = R1 - R2;                         // 112 v
    R2 = _gs_fo_rec;
    R1 = R1 + R2;
    DM(-2, I6) = R1;                      // the R record
    R1 = LSHIFT R0 BY 5;
    R2 = _gs_rbuf;
    R1 = R1 + R2;
    DM(-3, I6) = R1;                      // the R buffer
    I4 = R4;
    I0 = DM(-2, I6);
    R0 = DM(21, I4);
    DM(21, I0) = R0;
    R0 = DM(22, I4);
    DM(22, I0) = R0;
    R0 = DM(23, I4);
    DM(23, I0) = R0;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_fo_stock, _gs_fo_l);      // L: the stock call, the authoritative path
    MODIFY(I7, 1) (NW);
    R4 = DM(-2, I6);
    R8 = DM(-3, I6);
    R12 = R8;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_fo_stock, _gs_fo_r);      // R: the same function on the R record and the R buffer
    MODIFY(I7, 1) (NW);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_fo_bad:
    GS_REFUSE(_gs_cnt + 36);
    JUMP _gs_fo_stock;

// ---- U/Y recurrence (0xb81368, calls 0x1c959f early and 0x1c9b7a late): L record 8 words at engine + 0x12148 + 0x20 v; control words 2 to 5 ----
.GLOBAL _gs_uy;
_gs_uy:
    GS_CHECK(_gs_uy_stock);
    GS_SUPPORTED(_gs_uy_bad, 1);
    DK_ENTER(8);
    GS_SAVE_I0;
    R0 = I4;
    R1 = _gs_stamp;
    R0 = R0 - R1;
    R1 = LSHIFT R0 BY 3;                  // 32 v
    R2 = _gs_uy_rec;
    R1 = R1 + R2;
    DM(-2, I6) = R1;
    R1 = LSHIFT R0 BY 5;
    R2 = _gs_rbuf;
    R1 = R1 + R2;
    DM(-3, I6) = R1;
    I4 = R4;
    I0 = DM(-2, I6);
    R0 = DM(2, I4);
    DM(2, I0) = R0;
    R0 = DM(3, I4);
    DM(3, I0) = R0;
    R0 = DM(4, I4);
    DM(4, I0) = R0;
    R0 = DM(5, I4);
    DM(5, I0) = R0;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_uy_stock, _gs_uy_l);
    MODIFY(I7, 1) (NW);
    R4 = DM(-2, I6);
    R8 = DM(-3, I6);
    R12 = R8;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_uy_stock, _gs_uy_r);
    MODIFY(I7, 1) (NW);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_uy_bad:
    GS_REFUSE(_gs_cnt + 40);
    JUMP _gs_uy_stock;

// ---- the gain stage (0xb80345, call 0x1c99cc): L record 11 words at engine + 0x12508 + 0x2c v, all captured; stack arguments: control, byte control, count ----
.GLOBAL _gs_gain;
_gs_gain:
    GS_CHECK(_gs_gain_stock);
    GS_SUPPORTED(_gs_gain_bad, 3);
    DK_ENTER(8);
    GS_SAVE_I0;
    R0 = I4;
    R1 = _gs_stamp;
    R0 = R0 - R1;
    R1 = LSHIFT R0 BY 3;
    R2 = LSHIFT R0 BY 1;
    R1 = R1 + R2;
    R1 = R1 + R0;                         // 44 v
    R2 = _gs_gain_rec;
    R1 = R1 + R2;
    DM(-2, I6) = R1;
    R1 = LSHIFT R0 BY 5;
    R2 = _gs_rbuf;
    R1 = R1 + R2;
    DM(-3, I6) = R1;
    I4 = R4;
    I0 = DM(-2, I6);
    R1 = 11;
_gs_gain_cap:
    R0 = DM(0, I4);
    DM(0, I0) = R0;
    MODIFY(I4, 4);
    MODIFY(I0, 4);
    R1 = R1 - 1;
    IF NE JUMP _gs_gain_cap;
    R0 = DM(3, I6);
    DM(I7, M7) = R0;
    R0 = DM(2, I6);
    DM(I7, M7) = R0;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_gain_stock, _gs_gain_l);
    MODIFY(I7, 3) (NW);
    R4 = DM(-2, I6);
    R8 = DM(-3, I6);
    R12 = R8;
    R0 = DM(3, I6);
    DM(I7, M7) = R0;
    R0 = DM(2, I6);
    DM(I7, M7) = R0;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_gain_stock, _gs_gain_r);
    MODIFY(I7, 3) (NW);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_gain_bad:
    GS_REFUSE(_gs_cnt + 44);
    JUMP _gs_gain_stock;

// ---- dispatch case 5 (0xb81526, call 0x1c9c08) and the blend that follows it (0x1c9c12 to 0x1c9c44): L record 53 words at engine + 0x12a10... ----
// The record words 11 + 7k and 13 + 7k (k = 0 to 5) are the stage's coefficient ramps; words 11 to 48 are captured. The stage writes the scratch block
// R12 (the stock one is DM(engine + 0x13808), a firmware pointer); the R pass writes the slot's private R scratch. The blend takes its two ramps from
// the 14-word record at I0 (the caller's I0, kept by the stock call) and writes nothing to it: the blend of R runs here, on the R buffer.
.GLOBAL _gs_c5;
_gs_c5:
    GS_CHECK(_gs_c5_stock);
    GS_SUPPORTED(_gs_c5_bad, 1);
    DK_ENTER(16);
    GS_SAVE_I0;
    R0 = I1;
    DM(-5, I6) = R0;
    R0 = I5;
    DM(-6, I6) = R0;
    DM(-7, I6) = R3;
    DM(-8, I6) = R6;
    R0 = I4;
    R1 = _gs_stamp;
    R0 = R0 - R1;                         // 4 v
    R1 = 53;
    R1 = R1 * R0 (SSI);                   // 212 v
    R2 = _gs_c5_rec;
    R1 = R1 + R2;
    DM(-2, I6) = R1;                      // the R record
    R1 = LSHIFT R0 BY 5;
    R2 = _gs_rbuf;
    R1 = R1 + R2;
    DM(-3, I6) = R1;                      // the R buffer
    R1 = LSHIFT R0 BY 5;
    R2 = _gs_c5_scr;
    R1 = R1 + R2;
    DM(-9, I6) = R1;                      // the R scratch
    I4 = R4;
    I0 = DM(-2, I6);
    MODIFY(I4, 44);
    MODIFY(I0, 44);
    R1 = 38;
_gs_c5_cap:
    R0 = DM(0, I4);
    DM(0, I0) = R0;
    MODIFY(I4, 4);
    MODIFY(I0, 4);
    R1 = R1 - 1;
    IF NE JUMP _gs_c5_cap;
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_c5_stock, _gs_c5_l);
    MODIFY(I7, 1) (NW);
    R4 = DM(-2, I6);
    R8 = DM(-3, I6);
    R12 = DM(-9, I6);
    R0 = DM(1, I6);
    DM(I7, M7) = R0;
    DK_CALL(_gs_c5_stock, _gs_c5_r);
    MODIFY(I7, 1) (NW);
    // the blend of R (the stock loop 0x1c9c15 to 0x1c9c44; I0 = the caller's ramp record, R9 = 32)
    I0 = DM(-4, I6);
    I4 = DM(-3, I6);                      // the R buffer: input and output
    I5 = DM(-9, I6);                      // the R scratch
    I1 = DM(-3, I6);
    R2 = DM(0, I0);                       // ramp a: start, step
    R12 = DM(2, I0);
    R8 = DM(7, I0);                       // ramp b: start, step
    R0 = DM(9, I0);
    LCNTR = 32, DO _gs_c5_blend UNTIL LCE;
        R4 = DM(I5, M6);                  // scratch
        R3 = DM(I4, M6);                  // R buffer
        F4 = F2 * F4;
        F3 = F8 * F3;
        F6 = F4 + F3;
        F2 = F2 + F12;
        F8 = F8 + F0;
_gs_c5_blend: DM(I1, M6) = R6;
    I0 = DM(-4, I6);
    I1 = DM(-5, I6);
    I5 = DM(-6, I6);
    R3 = DM(-7, I6);
    R6 = DM(-8, I6);
    JUMP _gs_leave;
_gs_c5_bad:
    GS_REFUSE(_gs_cnt + 48);
    JUMP _gs_c5_stock;

// ---- the conditional shaper (0x1cdb56; early call 0x1c95c9, late call 0x1c9935): L record 24 words at engine + 0xe788 + 0x60 v ----
// The stage reads words 0, 1, 2, 5 and 12, writes 4, 5, 6, 11, 12 and 13 (two groups of old, current and step of a control), and keeps no audio: the R record is a
// copy of the five words it reads, taken before the L call, and holds no state. The late call is bracketed by the engine's own scalings (x16 before, x1/16 after, 0x1c98f9 to 0x1c9975
// for L): the R buffer gets the same two multiplications around the R pass.
#define GS_SHAPER_COPY \
    GS_COPY(0); GS_COPY(1); GS_COPY(2); GS_COPY(5); GS_COPY(12)

.GLOBAL _gs_shaper_early;
_gs_shaper_early:
    GS_TWIN_HEAD(_gs_shaper_stock, _gs_shaper_early_bad, 24, _gs_sh_rec);
    GS_SHAPER_COPY;
    GS_L1(_gs_shaper_stock, _gs_shaper_early_l);
    GS_R1(_gs_shaper_stock, _gs_shaper_early_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_shaper_early_bad:
    GS_REFUSE(_gs_cnt + 8);
    JUMP _gs_shaper_stock;

.GLOBAL _gs_shaper_late;
_gs_shaper_late:
    GS_TWIN_HEAD(_gs_shaper_stock, _gs_shaper_late_bad, 24, _gs_sh_rec);
    GS_SHAPER_COPY;
    GS_L1(_gs_shaper_stock, _gs_shaper_late_l);
    GS_SCALE_R(0x41800000, _gs_shaper_up);
    GS_R1(_gs_shaper_stock, _gs_shaper_late_r);
    GS_SCALE_R(0x3d800000, _gs_shaper_down);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_shaper_late_bad:
    GS_REFUSE(_gs_cnt + 12);
    JUMP _gs_shaper_stock;

// ---- the held-sample stage (0xb8265b; early call 0x1c95b4, late call 0x1c9b8f): L record 5 words at engine + 0xeec8 + 0x14 v ----
// Words 0 (the control), 3 and 4 (the phase counters) are shared: R gets the values L saw before its call, so both channels hold and refresh in the same
// samples. Words 1 and 2 (the held audio) are private to R.
.GLOBAL _gs_held_early;
_gs_held_early:
    GS_TWIN_HEAD(_gs_held_stock, _gs_held_early_bad, 5, _gs_hd_rec);
    GS_COPY(0);
    GS_COPY(3);
    GS_COPY(4);
    GS_L1(_gs_held_stock, _gs_held_early_l);
    GS_R1(_gs_held_stock, _gs_held_early_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_held_early_bad:
    GS_REFUSE(_gs_cnt + 0);
    JUMP _gs_held_stock;

.GLOBAL _gs_held_late;
_gs_held_late:
    GS_TWIN_HEAD(_gs_held_stock, _gs_held_late_bad, 5, _gs_hd_rec);
    GS_COPY(0);
    GS_COPY(3);
    GS_COPY(4);
    GS_L1(_gs_held_stock, _gs_held_late_l);
    GS_R1(_gs_held_stock, _gs_held_late_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_held_late_bad:
    GS_REFUSE(_gs_cnt + 4);
    JUMP _gs_held_stock;

// ---- dispatch cases 1, 2 and 3 (0xb82342 call 0x1c98af, 0xb81a16 call 0x1c9bb6, 0xb80f2e call 0x1c9bd1): in place on the voice buffer ----
// Only the words the stage reads are copied from the L record before the L call (the audited read sets of docs/findings/96); the audio histories are private to
// R. The call itself advances the shared control words of case 3 (current coefficients {17, 18, 19} + 7k and the prior values 46 and 53), so R starts from the
// state L saw.
// case 1: L record 51 words at engine + 0xf308 + 0xcc v; reads 3, 4 (histories), 5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42; writes 3, 4 and the snapshots 48 to 50.
.GLOBAL _gs_case1;
_gs_case1:
    GS_TWIN_HEAD(_gs_case1_stock, _gs_case1_bad, 51, _gs_c1_rec);
    GS_COPY(5); GS_COPY(6); GS_COPY(7); GS_COPY(12); GS_COPY(14); GS_COPY(26);
    GS_COPY(28); GS_COPY(33); GS_COPY(35); GS_COPY(40); GS_COPY(42);
    GS_L1(_gs_case1_stock, _gs_case1_l);
    GS_R1(_gs_case1_stock, _gs_case1_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_case1_bad:
    GS_REFUSE(_gs_cnt + 16);
    JUMP _gs_case1_stock;

// case 2: L record 75 words at engine + 0xffc8 + 0x12c v; reads 10, 11 (histories) and 12 + 7k, 14 + 7k for k = 0 to 8 (coefficient and step).
.GLOBAL _gs_case2;
_gs_case2:
    GS_TWIN_HEAD(_gs_case2_stock, _gs_case2_bad, 75, _gs_c2_rec);
    MODIFY(I4, 48);
    MODIFY(I0, 48);
    R1 = 9;
_gs_case2_cap:
    R0 = DM(0, I4);
    DM(0, I0) = R0;
    R0 = DM(2, I4);
    DM(2, I0) = R0;
    MODIFY(I4, 28);
    MODIFY(I0, 28);
    R1 = R1 - 1;
    IF NE JUMP _gs_case2_cap;
    GS_L1(_gs_case2_stock, _gs_case2_l);
    GS_R1(_gs_case2_stock, _gs_case2_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_case2_bad:
    GS_REFUSE(_gs_cnt + 20);
    JUMP _gs_case2_stock;

// case 3: L record 59 words at engine + 0x11288 + 0xec v; reads 5, 9, 10, 11, 12 (histories), 0, 1, 2, 3, 4, 14 (controls) and 18, 25, 32, 39, 46, 53 (current
// coefficients and the prior values); the call writes {17, 18, 19} + 7k for k = 0 to 5.
.GLOBAL _gs_case3;
_gs_case3:
    GS_TWIN_HEAD(_gs_case3_stock, _gs_case3_bad, 59, _gs_c3_rec);
    GS_COPY(0); GS_COPY(1); GS_COPY(2); GS_COPY(3); GS_COPY(4); GS_COPY(14);
    GS_COPY(18); GS_COPY(25); GS_COPY(32); GS_COPY(39); GS_COPY(46); GS_COPY(53);
    GS_L1(_gs_case3_stock, _gs_case3_l);
    GS_R1(_gs_case3_stock, _gs_case3_r);
    I0 = DM(-4, I6);
    JUMP _gs_leave;
_gs_case3_bad:
    GS_REFUSE(_gs_cnt + 24);
    JUMP _gs_case3_stock;

// ---- the ring stage (0xb806f5, dispatch case 4 call 0x1c9be9 and case 6 call 0x1c9c4f): L record 18 words at engine + 0x12348 + 0x48 v ----
// The stage keeps a 32 KiB delay ring behind the byte pointer in word 16 (L: 0x804ad790 + 0x8000 v, finding 106). R has its own ring: slot v uses the 32 KiB at
// ADM_RING_R_BASE + 0x8000 v ([adapter.sample_pool] ring_r_base, a range of the DSP's DDR that neither the stock engine nor the sample pool uses), and the twin writes
// that pointer to word 16 of the R record before every R pass. The DSP alone reads and writes the ring, as it does the stock rings: no cache maintenance.
// Reads 0 (history), 2, 4, 10 (controls), 13, 14 (read coordinate and write cursor), 15 (delay), 16, 17 (fill status); writes 0, 3, 4, 5, 13, 14, 17 and the ring. The
// R record gets 2, 3, 4, 5, 10 and 15 from L before the L call; words 0, 13, 14, 17 and the ring are R's own. R's note-on state is what the stock reset leaves in L:
// history and cursor 0, fill status 1, the ring backing as it is (the status makes the stage ignore it until the ring has been written once).
#define GS_RING_COPY \
    GS_COPY(2); GS_COPY(3); GS_COPY(4); GS_COPY(5); GS_COPY(10); GS_COPY(15)

#define GS_RING(NAME, REFUSE, CNT, LABEL_L, LABEL_R) \
.GLOBAL NAME; \
NAME: \
    GS_TWIN_HEAD(_gs_ring_stock, REFUSE, 18, _gs_rg_rec); \
    R1 = LSHIFT R0 BY 13; \
    R2 = ADM_RING_R_BASE; \
    R1 = R1 + R2; \
    DM(16, I0) = R1; \
    GS_RING_COPY; \
    GS_L1(_gs_ring_stock, LABEL_L); \
    GS_R1(_gs_ring_stock, LABEL_R); \
    I0 = DM(-4, I6); \
    JUMP _gs_leave; \
REFUSE: \
    GS_REFUSE(CNT); \
    JUMP _gs_ring_stock

GS_RING(_gs_ring4, _gs_ring4_bad, _gs_cnt + 28, _gs_ring4_l, _gs_ring4_r);
GS_RING(_gs_ring6, _gs_ring6_bad, _gs_cnt + 32, _gs_ring6_l, _gs_ring6_r);

// ---- the common return of a stereo twin: pops the twin's frame and returns to the caller of the retargeted call ----
_gs_leave:
    I12 = DM(-1, I6);
    NOP;
    NOP;
    NOP;
    JUMP (M14, I12) (DB);
    NOP;
    RFRAME;

// ---- reset mirrors: the stock reset also runs on the R record (zero the histories), whatever the stamp says ----
// First-order reset (0xb80b93, call 0x1c96cb): R4 = L record; R words 24 to 27.
.GLOBAL _gs_fo_reset;
_gs_fo_reset:
    R0 = _gs_engine_base;
    R1 = 0xe088;
    R0 = R0 + R1;
    R0 = R4 - R0;
    IF LT JUMP _gs_fo_reset_stock;
    R2 = R0;                              // the byte offset from slot 0
    R0 = LSHIFT R0 BY -4;                 // 7 v
    R1 = 37;
    R0 = R0 * R1 (SSI);
    R0 = LSHIFT R0 BY -8;                 // v
    R1 = 16;
    COMP(R0, R1);
    IF GE JUMP _gs_fo_reset_stock;
    R1 = 112;
    R0 = R0 * R1 (SSI);                   // 112 v
    COMP(R0, R2);
    IF NE JUMP _gs_fo_reset_stock;        // not the start of a slot's record
    R1 = _gs_fo_rec;
    R0 = R0 + R1;
    I4 = R0;
    R1 = 0;
    DM(24, I4) = R1;
    DM(25, I4) = R1;
    DM(26, I4) = R1;
    DM(27, I4) = R1;
    JUMP _gs_fo_reset_stock;

// U/Y reset (0xb8135a, call 0x1c96aa): R4 = L record; R words 6 and 7.
.GLOBAL _gs_uy_reset;
_gs_uy_reset:
    R0 = _gs_engine_base;
    R1 = 0x12148;
    R0 = R0 + R1;
    R0 = R4 - R0;
    IF LT JUMP _gs_uy_reset_stock;
    R2 = R0;
    R0 = LSHIFT R0 BY -5;                 // v
    R1 = 16;
    COMP(R0, R1);
    IF GE JUMP _gs_uy_reset_stock;
    R0 = LSHIFT R0 BY 5;                  // 32 v
    COMP(R0, R2);
    IF NE JUMP _gs_uy_reset_stock;        // not the start of a slot's record
    R1 = _gs_uy_rec;
    R0 = R0 + R1;
    I4 = R0;
    R1 = 0;
    DM(6, I4) = R1;
    DM(7, I4) = R1;
    JUMP _gs_uy_reset_stock;


// ---- the resets in the middle of a note: the same mirror for the other stages (sites in voice.toml: the engine's calls after a change of the SRR routing, the filter
// type or the ring delay; the note-on resets are covered by the clear of `_gs_post`) ----
// R4 = the L record. When it is the start of a slot's record (offset OFF from the engine base, STRIDE bytes per slot, exact quotient with the multiplier MULT >> 16), the R
// private words that the stock reset sets are set for R too, then the stock reset runs on L. Any other pointer reaches the stock reset only. Clobbers R0, R1, R2, I4.
#define GS_RESET_HEAD(STOCK, OFF, MULT, STRIDE, REC) \
    R0 = _gs_engine_base; \
    R1 = OFF; \
    R0 = R0 + R1; \
    R0 = R4 - R0; \
    IF LT JUMP STOCK; \
    R2 = R0; \
    R1 = MULT; \
    R0 = R0 * R1 (SSI); \
    R0 = LSHIFT R0 BY -16; \
    R1 = 16; \
    COMP(R0, R1); \
    IF GE JUMP STOCK; \
    R1 = STRIDE; \
    R0 = R0 * R1 (SSI); \
    COMP(R0, R2); \
    IF NE JUMP STOCK; \
    R1 = REC; \
    R0 = R0 + R1; \
    I4 = R0; \
    R1 = 0

// held sample (0xb82440, call 0x1c91c5): the stock reset zeroes words 0 to 4
.GLOBAL _gs_held_reset;
_gs_held_reset:
    GS_RESET_HEAD(_gs_held_reset_stock, 0xeec8, 3277, 20, _gs_hd_rec);
    DM(0, I4) = R1;
    DM(1, I4) = R1;
    DM(2, I4) = R1;
    DM(3, I4) = R1;
    DM(4, I4) = R1;
    JUMP _gs_held_reset_stock;

// case 1 (0xb81d6d, call 0x1c9716): histories 3 and 4 (the snapshots 48 to 50 survive a stock reset)
.GLOBAL _gs_case1_reset;
_gs_case1_reset:
    GS_RESET_HEAD(_gs_case1_reset_stock, 0xf308, 322, 204, _gs_c1_rec);
    DM(3, I4) = R1;
    DM(4, I4) = R1;
    JUMP _gs_case1_reset_stock;

// case 2 (0xb81c8d, call 0x1c9747): histories 10 and 11
.GLOBAL _gs_case2_reset;
_gs_case2_reset:
    GS_RESET_HEAD(_gs_case2_reset_stock, 0xffc8, 219, 300, _gs_c2_rec);
    DM(10, I4) = R1;
    DM(11, I4) = R1;
    JUMP _gs_case2_reset_stock;

// case 3 (0xb811f1, call 0x1c976f): histories 5, 9, 10, 11 and 12
.GLOBAL _gs_case3_reset;
_gs_case3_reset:
    GS_RESET_HEAD(_gs_case3_reset_stock, 0x11288, 278, 236, _gs_c3_rec);
    DM(5, I4) = R1;
    DM(9, I4) = R1;
    DM(10, I4) = R1;
    DM(11, I4) = R1;
    DM(12, I4) = R1;
    JUMP _gs_case3_reset_stock;

// case 5 (0xb818c7, call 0x1c9842): histories 9 and 10
.GLOBAL _gs_case5_reset;
_gs_case5_reset:
    GS_RESET_HEAD(_gs_case5_reset_stock, 0x127c8, 310, 212, _gs_c5_rec);
    DM(9, I4) = R1;
    DM(10, I4) = R1;
    JUMP _gs_case5_reset_stock;

// ring (0xb805d8, calls 0x1c97cd and 0x1c989f): history (word 0) and write cursor (word 14) to 0, fill status (word 17) to M14 as the stock reset stores it; the read
// coordinate (word 13) and the backing stay as they are, as in L
.GLOBAL _gs_ring_reset;
_gs_ring_reset:
    GS_RESET_HEAD(_gs_ring_reset_stock, 0x12348, 911, 72, _gs_rg_rec);
    DM(0, I4) = R1;
    DM(14, I4) = R1;
    R1 = M14;
    DM(17, I4) = R1;
    JUMP _gs_ring_reset_stock;

// ---- once per block, from the heartbeat code of slot 0 (dsp-voice-dn2.s): publishes the events of the block that has just ended ----
// Plain subroutine; clobbers R0 and R1 only.
.GLOBAL _gs_block;
_gs_block:
    R0 = DM(_gs_hacc);
    DM(_gs_health) = R0;
    R0 = 0;
    DM(_gs_hacc) = R0;
    RTS;

// ---- the two inline gain stages of the engine that have no call: the block ramp (0x1c99ed to 0x1c9a31) and the conditional fade (0x1c9a59 to 0x1c9aac) ----
// Hook at 0x1c9aaf (`BIT SET MODE1 0x200000`, the first instruction after the fade loop), once per block, before the metadata clear that zeroes the fade's flags.
// For every stamped slot the same two multiplications run on its R buffer in place:
//   ramp:  r = DM(rec), step = DM(rec + 2) of the record engine + 0x1395c + 28 v; out[k] = r_k * in[k], r += step after each sample;
//   fade:  when the byte engine + 0x138fc + v and the word engine + 0x1351c + 44 v are both nonzero (the engine's two flags): r from 1.0, step -1/31 (the
//          stock division 0x1c06ba of -1.0 by 31.0 gives that value, tested), out[k] = r_k * in[k].
// Live in at the hook: I0 (the metadata clear), M13, M5, M7, R9 = 32 and R11 = 0 (the stock code compares them next), I6. Everything else is dead: the
// caller-saved registers are clobbered by the stock epilogue's contract and the callee-saved ones are reloaded from the frame at 0x1c9b33.
.GLOBAL _gs_prepan;
_gs_prepan:
    R5 = 32;
    COMP(R9, R5);
    IF NE JUMP _gs_pp_done;
    R8 = DM(_dv_diag);                    // the heartbeat
    R15 = 0;                              // 4 v
_gs_pp_slot:
    R5 = _gs_stamp;
    R5 = R5 + R15;
    I1 = R5;
    R6 = DM(0, I1);
    COMP(R6, R8);
    IF NE JUMP _gs_pp_next;
    R14 = LSHIFT R15 BY 5;
    R5 = _gs_rbuf;
    R14 = R14 + R5;                       // the R buffer: 128 v
    R6 = _gs_engine_base;
    R7 = LSHIFT R15 BY 3;
    R7 = R7 - R15;                        // 28 v
    R6 = R6 + R7;
    R7 = 0x1395c;
    R6 = R6 + R7;
    I1 = R6;
    R10 = DM(0, I1);
    R11 = DM(2, I1);
    I2 = R14;
    LCNTR = 32, DO _gs_ramp UNTIL LCE;
        R4 = DM(0, I2);
        F4 = F10 * F4;
        F10 = F10 + F11;
_gs_ramp: DM(I2, M6) = R4;
    // flag 1 is byte v of the array at engine + 0x138fc (the stock read is a byte access, Type 3b (l, x, w) = (0, 0, 0)): the word that holds it, shifted
    R7 = LSHIFT R15 BY -2;                // v
    R6 = 12;
    R6 = R7 AND R6;                       // 4 * (v >> 2)
    R5 = _gs_engine_base;
    R5 = R5 + R6;
    R6 = 0x138fc;
    R5 = R5 + R6;
    I1 = R5;
    R4 = DM(0, I1);
    R5 = 3;
    R5 = R7 AND R5;
    R5 = LSHIFT R5 BY 3;                  // 8 * (v & 3)
    R6 = 0;
    R5 = R6 - R5;
    R4 = LSHIFT R4 BY R5;
    R5 = 0xff;
    R4 = R4 AND R5;
    R4 = PASS R4;
    IF EQ JUMP _gs_pp_next;
    R6 = _gs_engine_base;
    R7 = LSHIFT R15 BY 3;
    R5 = LSHIFT R15 BY 1;
    R7 = R7 + R5;
    R7 = R7 + R15;                        // 44 v
    R6 = R6 + R7;
    R7 = 0x1351c;
    R6 = R6 + R7;
    I1 = R6;
    R4 = DM(0, I1);
    R4 = LEFTZ R4;
    IF SV JUMP _gs_pp_next;
    I2 = R14;
    R10 = 0x3f800000;
    R11 = 0xbd042108;                     // -1/31 as the stock division 0x1c06ba returns it
    LCNTR = 32, DO _gs_fade UNTIL LCE;
        R4 = DM(0, I2);
        F4 = F10 * F4;
        F10 = F10 + F11;
_gs_fade: DM(I2, M6) = R4;
_gs_pp_next:
    R5 = 4;
    R15 = R15 + R5;
    R5 = 64;
    COMP(R15, R5);
    IF LT JUMP _gs_pp_slot;
_gs_pp_done:
    BIT SET MODE1 0x200000;               // the displaced instruction
    JUMP _gs_pre_cont;

// ---- the pan loop of the engine (0x1c9aea to 0x1c9b30): hook at 0x1c9af6 (the multiply of plane A's first sample), once per slot ----
// At the hook: R0 = 31 (R9 - 1), R2 = the pan gain A of sample 0, R8 = L[0], I2 = L + 1, I3 = the slot's entry in the table of voice buffers (the table
// is frame word -24), I4 = the slot's pan record (stepped by 7 words after the read of R2), I5 = the slot's entry in the table of plane pointers (A, B),
// M4 = plane A, R9 = 32. Dead in the loop (the epilogue reloads them): R1, R3 to R7, R10, R11, R13 to R15, I0, I1, I8 to I11, I14, I15, M0 to M3, M8 to M11
// (a DM access needs I0 to I7: I0, I1 and, until the end of the slot, I3 are the pointers here). R12 (the return address of the engine) is not touched.
//
// A stamped slot: plane A = gA * L as in the stock loop, plane B = gB * R (R has been through the ramp and the fade, `_gs_prepan`). Every other slot: the
// displaced instruction and the stock loop.
.GLOBAL _gs_slot;
_gs_slot:
    R3 = I3;
    R5 = DM(-24, I6);
    R3 = R3 - R5;                         // 4 v
    R5 = _gs_stamp;
    R5 = R5 + R3;
    I1 = R5;
    R6 = DM(0, I1);
    R7 = DM(_dv_diag);
    COMP(R6, R7);
    IF NE JUMP _gs_mono;
    R7 = 31;
    COMP(R0, R7);
    IF NE JUMP _gs_mono;
    R10 = R3;                             // 4 v, kept for the slot's end
    R14 = LSHIFT R3 BY 5;
    R5 = _gs_rbuf;
    R14 = R14 + R5;                       // the R buffer
    I3 = R14;
    R13 = DM(1, I5);                      // plane B
    I0 = M4;                              // plane A
    I1 = R13;
    R12 = DM(-5, I4);                     // step A
    R13 = DM(0x6b, I4);                   // step B
    R1 = DM(0x69, I4);                    // gain B of sample 0
    R7 = DM(I3, M6);                      // R[0]
    F4 = F2 * F8;
    F15 = F1 * F7;
    DM(I0, M6) = R4;
    DM(I1, M6) = R15;
    LCNTR = 31, DO _gs_pan UNTIL LCE;
        F2 = F2 + F12, R3 = DM(I2, M6);
        F1 = F1 + F13, R5 = DM(I3, M6);
        F6 = F2 * F3;
        F7 = F1 * F5, DM(I0, M6) = R6;
_gs_pan: DM(I1, M6) = R7;
    R3 = DM(-24, I6);                     // I3 of the next slot: the entry of this one plus one word
    R3 = R3 + R10;
    I3 = R3;
    MODIFY(I3, 4);                        // the stock `modify(I3, M6)`: one word
    JUMP _gs_pan_end;
_gs_mono:
    I1 = 0;                               // I1 as at the hook (the check used it); two instructions before its use, as the stock code has
    R0 = R9 - 1;                          // R0 and the flags as at the hook
    F4 = F2 * F8;                         // the displaced instruction `F4 = F2 * F8; modify(I1, M4)`
    MODIFY(I1, M4);
    JUMP _gs_pan_cont;

// ---- the adapter's post-step (CALL from _dv_contract after the guard of L), plain subroutine: R is checked and scaled, the slot stamped ----
// In: _dv_c + 4 = the slot v, _dv_c + 48 = the status bit DK_STEREO of the module's return (0: mono), _dv_in + DK_IN_BLOCK. May clobber R0-R15, I0-I5.
// A bad R block (not finite, over 8.0 in magnitude) or a slot over the cap stays unstamped: the slot plays R = L for the block.
.GLOBAL _gs_post;
_gs_post:
    R2 = DM(_dv_c + 48);
    R2 = PASS R2;
    IF EQ RTS;
    R4 = DM(_dv_c + 4);
    // the slot is stereo in this block, whatever the checks below say (read by the stages without a twin: `_gs_seen`)
    R1 = LSHIFT R4 BY 2;
    R0 = _gs_seen;
    R0 = R0 + R1;
    I3 = R0;
    R0 = DM(_dv_diag);
    DM(0, I3) = R0;
    R5 = LSHIFT R4 BY 7;
    R6 = _gs_rbuf;
    R5 = R5 + R6;
    I0 = R5;
    R13 = 0;
    R9 = 0x7fffffff;
    LCNTR = 32, DO _gs_gchk UNTIL LCE;
        R0 = DM(I0, M6);
        R0 = R0 AND R9;
_gs_gchk: R13 = MAX(R13, R0);
    R0 = 0x41000000;
    COMP(R13, R0);
    IF GT JUMP _gs_badr;
    // the cap: the first _GS_MAX_STEREO stereo slots of a block
    R0 = DM(_dv_diag);
    R1 = DM(_gs_nhb);
    COMP(R0, R1);
    IF EQ JUMP _gs_samehb;
    DM(_gs_nhb) = R0;
    R1 = 0;
    DM(_gs_ns) = R1;
_gs_samehb:
    R1 = DM(_gs_ns);
    R2 = 8;
    COMP(R1, R2);
    IF GE JUMP _gs_capped;
    R1 = R1 + 1;
    DM(_gs_ns) = R1;
    // the engine multiplies every voice buffer by 0x3fcade01 right after the render hook (the loop at 0x1c9478); R takes the same factor
    I0 = R5;
    I1 = R5;
    R2 = 0x3fcade01;
    LCNTR = 32, DO _gs_scale UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 * F2;
_gs_scale: DM(I1, M6) = R0;
    // the stage states of R restart at a note-on block and when R did not run in the previous block
    R1 = LSHIFT R4 BY 2;
    R2 = _gs_stamp;
    R2 = R2 + R1;
    I2 = R2;
    R3 = DM(0, I2);                       // the slot's stamp so far
    R0 = DM(_dv_diag);
    R7 = DM(_dv_in + DK_IN_BLOCK);
    R7 = PASS R7;
    IF EQ JUMP _gs_clear;
    R0 = R0 - 1;
    COMP(R3, R0);
    IF EQ JUMP _gs_stamped;
    R1 = DM(_gs_cnt + 72);                // a gap in a running voice (bad R, cap or a refusal in the last block): R restarts
    R1 = R1 + 1;
    DM(_gs_cnt + 72) = R1;
_gs_clear:
    R0 = 0;
    R1 = 112;
    R1 = R1 * R4 (SSI);
    R2 = _gs_fo_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 28, DO _gs_z1 UNTIL LCE;
_gs_z1: DM(I3, M6) = R0;
    R1 = 32;
    R1 = R1 * R4 (SSI);
    R2 = _gs_uy_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 8, DO _gs_z2 UNTIL LCE;
_gs_z2: DM(I3, M6) = R0;
    R1 = 44;
    R1 = R1 * R4 (SSI);
    R2 = _gs_gain_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 11, DO _gs_z3 UNTIL LCE;
_gs_z3: DM(I3, M6) = R0;
    R1 = 212;
    R1 = R1 * R4 (SSI);
    R2 = _gs_c5_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 53, DO _gs_z4 UNTIL LCE;
_gs_z4: DM(I3, M6) = R0;
    R1 = 20;
    R1 = R1 * R4 (SSI);
    R2 = _gs_hd_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 5, DO _gs_z5 UNTIL LCE;
_gs_z5: DM(I3, M6) = R0;
    R1 = 204;
    R1 = R1 * R4 (SSI);
    R2 = _gs_c1_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 51, DO _gs_z6 UNTIL LCE;
_gs_z6: DM(I3, M6) = R0;
    R1 = 300;
    R1 = R1 * R4 (SSI);
    R2 = _gs_c2_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 75, DO _gs_z7 UNTIL LCE;
_gs_z7: DM(I3, M6) = R0;
    R1 = 236;
    R1 = R1 * R4 (SSI);
    R2 = _gs_c3_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 59, DO _gs_z8 UNTIL LCE;
_gs_z8: DM(I3, M6) = R0;
    R1 = 72;
    R1 = R1 * R4 (SSI);
    R2 = _gs_rg_rec;
    R1 = R1 + R2;
    I3 = R1;
    LCNTR = 18, DO _gs_z9 UNTIL LCE;
_gs_z9: DM(I3, M6) = R0;
    R0 = 1;
    DM(-1, I3) = R0;                      // the fill status of a ring after the stock reset (word 17); the ring's backing is not cleared
_gs_stamped:
    R0 = DM(_dv_diag);
    DM(0, I2) = R0;
    RTS;

// a slot that is stereo but not stamped plays R = L: counted (`_gs_cnt` 64 = bad R, 68 = cap) and raised in `_gs_hacc` (bit 0 any reason, bit 2 bad R, bit 3 cap)
_gs_badr:
    R1 = DM(_gs_cnt + 64);
    R1 = R1 + 1;
    DM(_gs_cnt + 64) = R1;
    R2 = 5;
    JUMP _gs_raise;
_gs_capped:
    R1 = DM(_gs_cnt + 68);
    R1 = R1 + 1;
    DM(_gs_cnt + 68) = R1;
    R2 = 9;
_gs_raise:
    R1 = DM(_gs_hacc);
    R1 = R1 OR R2;
    DM(_gs_hacc) = R1;
    RTS;

.SECTION/DM seg_dmda;
.PKDATA _gs_stamp, 64;                     // per slot: the block (heartbeat) in which the slot has R; 0 or an older block: R = L
.PKDATA _gs_rbuf, 2048;                    // per slot: 32 floats of R (a machine's out_r)
.PKDATA _gs_fo_rec, 1792;                  // R state records, per slot: first-order 28 words, U/Y 8, gain 11, case 5 53 words; R scratch of case 5: 32 words
.PKDATA _gs_uy_rec, 512;
.PKDATA _gs_gain_rec, 704;
.PKDATA _gs_c5_rec, 3392;
.PKDATA _gs_c5_scr, 2048;
.PKDATA _gs_sh_rec, 1536;                  // R records of the shaper (24 words) and the held-sample stage (5 words), per slot
.PKDATA _gs_hd_rec, 320;
.PKDATA _gs_c1_rec, 3264;                  // R records of dispatch case 1 (51 words), 2 (75) and 3 (59), per slot
.PKDATA _gs_c2_rec, 4800;
.PKDATA _gs_c3_rec, 3776;
.PKDATA _gs_rg_rec, 1152;                   // R records of the ring stage (18 words), per slot; the R rings are at ADM_RING_R_BASE
.PKDATA _gs_ns, 4;                         // stereo slots stamped in the block _gs_nhb
.PKDATA _gs_nhb, 4;
.PKDATA _gs_nbad, 4;                       // stamps the guards of the twins have cleared (diagnostic)
.PKDATA _gs_seen, 64;                      // per slot: the block (heartbeat) in which the slot was stereo at adapter time, whether or not R was usable (never cleared)
.PKDATA _gs_cnt, 128;                      // 32 counters: 0 to 12 refusals of an unsupported context by a twin (held early and late, shaper early and late, case 1, 2, 3, ring 4 and 6, first-order, U/Y, gain, case 5), 16 bad R, 17 cap, 18 gap (R restarted in a running voice)
.PKDATA _gs_hacc, 4;                       // events of the running block: bit 0 some stereo slot plays R = L, bit 1 a twin refused an unsupported context, bit 2 bad R, bit 3 cap
.PKDATA _gs_health, 4;                     // `_gs_hacc` of the last completed block, published by `_gs_block` at the heartbeat of slot 0
