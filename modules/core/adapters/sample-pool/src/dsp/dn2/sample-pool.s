#include "dsp/call.inc"
#include "sample-pool-dsp-dn2.inc"
// core/sample-pool on the Digitone II 1.12: the DSP end of the page transport (docs/design/dn2-sample-pool.md section 2).
//
// The ColdFire sends 4,100-byte LP0 frames: a tag word and 1,024 payload words, one 4 KiB page of one plane of a sample. The LP0 driver
// (stock) fills an armed receive descriptor and calls _sp_callback in interrupt context. The callback checks the tag, copies the page into
// DDR through the NW32 alias (physical 0x80000000 + 4 * (w - 0x10000000)) and arms the descriptor again. Tag, rules and acknowledgement are in
// src/cf/pool_wire.h (the C reference of the rules is spw_dsp_rule):
//   tag = 0xD0000000 | seq8 << 20 | page_index20        the page index counts 4 KiB pages from the start of the pool range
//   accept   prefix 0xD, page index < SP_DDR_PAGES, seq == last seq + 1 (mod 256): copy, count, remember the tag
//   repeat   the tag equals the last accepted tag: copy again (the same data), no count
//   error 4  a bad prefix or a page index outside the range: nothing is written
//   error 5  any other sequence number: nothing is written
// An error leaves the receiver armed; the next good page clears it.
//
// The acknowledgement is a 16-byte extension of the reply record, written by _sp_tail in every block (the ColdFire wire is 0xabc + 16
// bytes, patched at boot). _sp_tail is called by core/dsp-link at the end of its block tail (dsp-link has the one claim on the tail); it
// saves every register itself, sets up the LP0 port once and returns to the tail.
//
// Not here, by design: any cache maintenance (the receive window is noncacheable through boot-window.s; PCM in DDR has one user, the core, which
// reads it through the cache it writes it with, finding 95 and docs/design/dn2-sample-pool.md section 3).
.SECTION/PM seg_pmco;
.GLOBAL _sp_tail;
.GLOBAL _sp_callback;
.EXTERN _sp_back;
.EXTERN _sp_open;
.EXTERN _sp_set_callback;
.EXTERN _sp_submit;
.EXTERN _sp_port_record;
.EXTERN _sp_descriptor;
.EXTERN _sp_state;
.EXTERN _sp_rx;
.EXTERN _sp_boot_status;

// Interrupts off around a snapshot or the callback's rearm: the local mask of the SDK, exclusion between the callback and the block tail.
#define SP_LOCK(SAVED) \
    SAVED = MODE1; \
    BIT CLR MODE1 0x1000; \
    NOP; \
    NOP
#define SP_UNLOCK(SAVED) \
    MODE1 = SAVED; \
    NOP; \
    NOP

_sp_tail:
    DM(_sp_save + 0) = R0;
    DM(_sp_save + 4) = R1;
    DM(_sp_save + 8) = R2;
    DM(_sp_save + 12) = R3;
    DM(_sp_save + 16) = R4;
    DM(_sp_save + 20) = R5;
    DM(_sp_save + 24) = R6;
    DM(_sp_save + 28) = R7;
    DM(_sp_save + 32) = R8;
    DM(_sp_save + 36) = R9;
    DM(_sp_save + 40) = R10;
    DM(_sp_save + 44) = R11;
    DM(_sp_save + 48) = R12;
    DM(_sp_save + 52) = R13;
    DM(_sp_save + 56) = R14;
    DM(_sp_save + 60) = R15;
    R0 = I0;
    DM(_sp_save + 64) = R0;
    R0 = I1;
    DM(_sp_save + 68) = R0;
    R0 = I2;
    DM(_sp_save + 72) = R0;
    R0 = I3;
    DM(_sp_save + 76) = R0;
    R0 = I4;
    DM(_sp_save + 80) = R0;
    R0 = I5;
    DM(_sp_save + 84) = R0;
    R0 = I6;
    DM(_sp_save + 88) = R0;
    R0 = I7;
    DM(_sp_save + 92) = R0;
    R0 = I8;
    DM(_sp_save + 96) = R0;
    R0 = I9;
    DM(_sp_save + 100) = R0;
    R0 = I10;
    DM(_sp_save + 104) = R0;
    R0 = I11;
    DM(_sp_save + 108) = R0;
    R0 = I12;
    DM(_sp_save + 112) = R0;
    R0 = I13;
    DM(_sp_save + 116) = R0;
    R0 = I14;
    DM(_sp_save + 120) = R0;
    R0 = I15;
    DM(_sp_save + 124) = R0;
    R0 = M0;
    DM(_sp_save + 128) = R0;
    R0 = M1;
    DM(_sp_save + 132) = R0;
    R0 = M2;
    DM(_sp_save + 136) = R0;
    R0 = M3;
    DM(_sp_save + 140) = R0;
    R0 = M4;
    DM(_sp_save + 144) = R0;
    R0 = M5;
    DM(_sp_save + 148) = R0;
    R0 = M6;
    DM(_sp_save + 152) = R0;
    R0 = M7;
    DM(_sp_save + 156) = R0;
    R0 = M8;
    DM(_sp_save + 160) = R0;
    R0 = M9;
    DM(_sp_save + 164) = R0;
    R0 = M10;
    DM(_sp_save + 168) = R0;
    R0 = M11;
    DM(_sp_save + 172) = R0;
    R0 = M12;
    DM(_sp_save + 176) = R0;
    R0 = M13;
    DM(_sp_save + 180) = R0;
    R0 = M14;
    DM(_sp_save + 184) = R0;
    R0 = M15;
    DM(_sp_save + 188) = R0;
    R0 = L0;
    DM(_sp_save + 192) = R0;
    R0 = L1;
    DM(_sp_save + 196) = R0;
    R0 = L2;
    DM(_sp_save + 200) = R0;
    R0 = L3;
    DM(_sp_save + 204) = R0;
    R0 = L4;
    DM(_sp_save + 208) = R0;
    R0 = L5;
    DM(_sp_save + 212) = R0;
    R0 = L6;
    DM(_sp_save + 216) = R0;
    R0 = L7;
    DM(_sp_save + 220) = R0;
    R0 = B0;
    DM(_sp_save + 224) = R0;
    R0 = B1;
    DM(_sp_save + 228) = R0;
    R0 = B2;
    DM(_sp_save + 232) = R0;
    R0 = B3;
    DM(_sp_save + 236) = R0;
    R0 = B4;
    DM(_sp_save + 240) = R0;
    R0 = B5;
    DM(_sp_save + 244) = R0;
    R0 = B6;
    DM(_sp_save + 248) = R0;
    R0 = B7;
    DM(_sp_save + 252) = R0;
    R0 = MR0F;
    DM(_sp_save + 256) = R0;
    R0 = MR1F;
    DM(_sp_save + 260) = R0;
    R0 = MR2F;
    DM(_sp_save + 264) = R0;
    R0 = MR0B;
    DM(_sp_save + 268) = R0;
    R0 = MR1B;
    DM(_sp_save + 272) = R0;
    R0 = MR2B;
    DM(_sp_save + 276) = R0;
    R0 = MODE1;
    DM(_sp_save + 280) = R0;
    R0 = LCNTR;
    DM(_sp_save + 284) = R0;
    R0 = STKYX;
    DM(_sp_save + 288) = R0;
    // Scalar conditions must not combine a stale secondary SIMD status flag.
    // Saved MODE1 (including bank selectors) is restored at the common exit.
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    // The LP0 port is set up once, from the first block that gets here. Every later block only reports.
    R0 = DM(_sp_started);
    R0 = PASS R0;
    IF NE JUMP _sp_reply;
    R0 = DM(_sp_boot_status);
    R1 = 1; COMP(R0, R1); IF NE JUMP _sp_bad_boot;
    R0 = IMASKP;
    R0 = PASS R0;
    IF NE JUMP _sp_bad_context;
    M5 = 0;
    M6 = 1;
    M7 = -1;
    M13 = 0;
    M14 = 1;
    M15 = -1;
    L0 = 0;
    L1 = 0;
    L2 = 0;
    L3 = 0;
    L4 = 0;
    L5 = 0;
    L6 = 0;
    L7 = 0;
    BIT CLR MODE1 0x20A000;
    BIT SET MODE1 0x10000;
    NOP; NOP;
    // Refuse an owner or active hardware BEFORE any firmware driver mutation.
    R0 = _sp_port_record;
    I4 = R0;
    R0 = DM(5, I4);
    R0 = PASS R0;
    IF NE JUMP _sp_bad_owned;
    I4 = 0x30ffe000;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF NE JUMP _sp_bad_owned;
    I4 = 0x30fff008;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF NE JUMP _sp_bad_owned;
    R0 = 1;
    DM(_sp_started) = R0;
    R12 = DM(_sp_state);
    R4 = 0;
    R8 = 1;
    R0 = _sp_handle;
    DM(I7, M7) = R0;
    R0 = 128;
    DM(I7, M7) = R0;
    DK_CALL(_sp_open, _sp_after_open);
    MODIFY(I7, 2) (NW);
    R0 = PASS R0;
    IF NE JUMP _sp_bad_driver;
    R4 = DM(_sp_handle);
    R0 = _sp_port_record;
    COMP(R4, R0);
    IF NE JUMP _sp_bad_driver;
    R8 = _sp_callback;
    R12 = _sp_status;
    DK_CALL(_sp_set_callback, _sp_after_callback);
    DK_CALL(_sp_arm, _sp_after_arm);
    // armed: the ColdFire may send from here on. A failed submit has set the status to dead.
    R0 = DM(_sp_status);
    R0 = PASS R0;
    IF NE JUMP _sp_reply;
    R0 = 1;
    DM(_sp_status) = R0;
    JUMP _sp_reply;
_sp_bad_boot:
    R0 = 8;
    JUMP _sp_dead;
_sp_bad_context:
    R0 = 1;
    JUMP _sp_dead;
_sp_bad_owned:
    R0 = 2;
    JUMP _sp_dead;
_sp_bad_driver:
    R0 = 3;
_sp_dead:
    DM(_sp_error) = R0;
    R0 = 3;
    DM(_sp_status) = R0;
    R0 = 1;
    DM(_sp_started) = R0;
_sp_reply:
    // Publish the acknowledgement: eight numeric halves, a word is its low half then its high half (src/cf/pool_wire.h).
    // The snapshot is taken with the callback masked, so it never sees a page half done. The current stock reply record is the one this
    // block's tail has just built; its extension starts 0xabc bytes in.
    SP_LOCK(R8);
    BIT CLR MODE1 0x200000;               // scalar stores; keep bank selectors
    NOP; NOP;
    M6 = 1;
    L4 = 0;
    L0 = 0;
    R0 = DM(_sp_status);                  // h2
    R1 = DM(_sp_errcount);
    R2 = 255;
    R1 = R1 AND R2;
    R1 = LSHIFT R1 BY 8;
    R2 = DM(_sp_error);
    R3 = 255;
    R2 = R2 AND R3;
    R1 = R1 OR R2;                        // h3 = error | errcount << 8
    R2 = DM(_sp_pages);                   // h4, h5 = the count
    R3 = DM(_sp_last);
    R4 = FEXT R3 BY 20:8;
    R4 = LSHIFT R4 BY 8;
    R5 = 255;
    R3 = R3 AND R5;
    R3 = R3 OR R4;                        // h6 = seq8 << 8 | page index & 0xff
    R4 = 0x5352;                          // h0 + h1
    R4 = R4 + R0;
    R4 = R4 + R1;
    R5 = 0xffff;
    R6 = R2 AND R5;
    R4 = R4 + R6;
    R6 = LSHIFT R2 BY -16;
    R4 = R4 + R6;
    R4 = R4 + R3;
    R4 = R4 AND R5;
    R6 = 0x5a5a;
    R4 = R4 XOR R6;                       // h7
    R5 = LSHIFT R1 BY 16;
    R0 = R0 OR R5;                        // word 1: status | h3 << 16
    R4 = LSHIFT R4 BY 16;
    R3 = R3 OR R4;                        // word 3: h6 | h7 << 16
    R7 = 0x00025350;                      // word 0: magic, version 2
    R1 = DM(_sp_save + 80);               // incoming I4: current reply byte pointer
    R4 = 0xabc;
    R1 = R1 + R4;
    I4 = R1;
    NOP; NOP; NOP;
    DM(I4, M6) = R7;
    DM(I4, M6) = R0;
    DM(I4, M6) = R2;
    DM(I4, M6) = R3;
    SP_UNLOCK(R8);
    // A B write also loads its I register: restore all bases before indices.
    R0 = DM(_sp_save + 128);
    M0 = R0;
    R0 = DM(_sp_save + 132);
    M1 = R0;
    R0 = DM(_sp_save + 136);
    M2 = R0;
    R0 = DM(_sp_save + 140);
    M3 = R0;
    R0 = DM(_sp_save + 144);
    M4 = R0;
    R0 = DM(_sp_save + 148);
    M5 = R0;
    R0 = DM(_sp_save + 152);
    M6 = R0;
    R0 = DM(_sp_save + 156);
    M7 = R0;
    R0 = DM(_sp_save + 160);
    M8 = R0;
    R0 = DM(_sp_save + 164);
    M9 = R0;
    R0 = DM(_sp_save + 168);
    M10 = R0;
    R0 = DM(_sp_save + 172);
    M11 = R0;
    R0 = DM(_sp_save + 176);
    M12 = R0;
    R0 = DM(_sp_save + 180);
    M13 = R0;
    R0 = DM(_sp_save + 184);
    M14 = R0;
    R0 = DM(_sp_save + 188);
    M15 = R0;
    R0 = DM(_sp_save + 192);
    L0 = R0;
    R0 = DM(_sp_save + 196);
    L1 = R0;
    R0 = DM(_sp_save + 200);
    L2 = R0;
    R0 = DM(_sp_save + 204);
    L3 = R0;
    R0 = DM(_sp_save + 208);
    L4 = R0;
    R0 = DM(_sp_save + 212);
    L5 = R0;
    R0 = DM(_sp_save + 216);
    L6 = R0;
    R0 = DM(_sp_save + 220);
    L7 = R0;
    R0 = DM(_sp_save + 224);
    B0 = R0;
    R0 = DM(_sp_save + 228);
    B1 = R0;
    R0 = DM(_sp_save + 232);
    B2 = R0;
    R0 = DM(_sp_save + 236);
    B3 = R0;
    R0 = DM(_sp_save + 240);
    B4 = R0;
    R0 = DM(_sp_save + 244);
    B5 = R0;
    R0 = DM(_sp_save + 248);
    B6 = R0;
    R0 = DM(_sp_save + 252);
    B7 = R0;
    R0 = DM(_sp_save + 256);
    MR0F = R0;
    R0 = DM(_sp_save + 260);
    MR1F = R0;
    R0 = DM(_sp_save + 264);
    MR2F = R0;
    R0 = DM(_sp_save + 268);
    MR0B = R0;
    R0 = DM(_sp_save + 272);
    MR1B = R0;
    R0 = DM(_sp_save + 276);
    MR2B = R0;
    R0 = DM(_sp_save + 288);
    STKYX = R0;
    R0 = DM(_sp_save + 284);
    LCNTR = R0;
    R0 = DM(_sp_save + 64);
    I0 = R0;
    R0 = DM(_sp_save + 68);
    I1 = R0;
    R0 = DM(_sp_save + 72);
    I2 = R0;
    R0 = DM(_sp_save + 76);
    I3 = R0;
    R0 = DM(_sp_save + 80);
    I4 = R0;
    R0 = DM(_sp_save + 84);
    I5 = R0;
    R0 = DM(_sp_save + 96);
    I8 = R0;
    R0 = DM(_sp_save + 100);
    I9 = R0;
    R0 = DM(_sp_save + 104);
    I10 = R0;
    R0 = DM(_sp_save + 108);
    I11 = R0;
    R0 = DM(_sp_save + 112);
    I12 = R0;
    R0 = DM(_sp_save + 116);
    I13 = R0;
    R0 = DM(_sp_save + 120);
    I14 = R0;
    R0 = DM(_sp_save + 124);
    I15 = R0;
    R0 = DM(_sp_save + 88);
    I6 = R0;
    R0 = DM(_sp_save + 92);
    I7 = R0;
    R0 = DM(_sp_save + 280);
    MODE1 = R0;
    NOP; NOP;
    R0 = DM(_sp_save + 0);
    R1 = DM(_sp_save + 4);
    R2 = DM(_sp_save + 8);
    R3 = DM(_sp_save + 12);
    R4 = DM(_sp_save + 16);
    R5 = DM(_sp_save + 20);
    R6 = DM(_sp_save + 24);
    R7 = DM(_sp_save + 28);
    R8 = DM(_sp_save + 32);
    R9 = DM(_sp_save + 36);
    R10 = DM(_sp_save + 40);
    R11 = DM(_sp_save + 44);
    R12 = DM(_sp_save + 48);
    R13 = DM(_sp_save + 52);
    R14 = DM(_sp_save + 56);
    R15 = DM(_sp_save + 60);
    JUMP _sp_back;

// Normal stock calling convention. Rebuild all seven words before each submit.
_sp_arm:
    DK_ENTER(1);
    DK_SAVE(2, R3);
    I4 = DM(_sp_descriptor);
    R0 = 0;
    DM(0, I4) = R0;
    R0 = DM(_sp_rx);
    DM(1, I4) = R0;
    R0 = 0x00100000;
    DM(2, I4) = R0;
    R0 = 0x401;
    DM(3, I4) = R0;
    R0 = 4;
    DM(4, I4) = R0;
    R0 = 0;
    DM(5, I4) = R0;
    DM(6, I4) = R0;
    R4 = DM(_sp_handle);
    R8 = DM(_sp_descriptor);
    R12 = 1;
    DM(I7, M7) = R0;
    DK_CALL(_sp_submit, _sp_after_submit);
    MODIFY(I7, 1) (NW);
    R0 = PASS R0;
    IF EQ JUMP _sp_arm_done;
    R0 = 9;
    DM(_sp_error) = R0;
    R0 = 3;
    DM(_sp_status) = R0;
_sp_arm_done:
    DK_RETADDR;
    NOP; NOP; NOP;
    DK_LEAVE(R3, 2);
// Callback arguments: R4=context (&_sp_status), R8=event (4), R12=normalized RX byte address. The stock driver calls it in interrupt context.
_sp_callback:
    DK_ENTER(10);
    DK_SAVE(2, R3);
    DK_SAVE(3, R5);
    DK_SAVE(4, R6);
    DK_SAVE(5, R7);
    R0 = I0;
    DK_SAVE(6, R0);
    R0 = I1;
    DK_SAVE(7, R0);
    R0 = I2;
    DK_SAVE(8, R0);
    R0 = I3;
    DK_SAVE(9, R0);
    R0 = I5;
    DK_SAVE(10, R0);
    SP_LOCK(R0);
    DK_SAVE(11, R0);
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    R0 = DM(_sp_status);
    R1 = 1;
    COMP(R0, R1);
    IF EQ JUMP _sp_cb_active;
    R1 = 2;
    COMP(R0, R1);
    IF EQ JUMP _sp_cb_active;
    R1 = 3;
    COMP(R0, R1);
    IF EQ JUMP _sp_cb_done;               // dead: ignore, no rearm
    JUMP _sp_cb_bad;                      // not armed yet: this callback should not exist
_sp_cb_active:
    R0 = _sp_status;
    COMP(R4, R0);
    IF NE JUMP _sp_cb_bad;
    R0 = 4;
    COMP(R8, R0);
    IF NE JUMP _sp_cb_bad;
    R0 = DM(_sp_rx);
    COMP(R12, R0);
    IF NE JUMP _sp_cb_bad;
    I0 = R0;
    R4 = DM(0, I0);                       // the tag
    // The rules of src/cf/pool_wire.h spw_dsp_rule, before any destination is derived.
    R0 = LSHIFT R4 BY -28;
    R1 = 13;
    COMP(R0, R1);
    IF NE JUMP _sp_cb_tagerr;
    R5 = FEXT R4 BY 0:20;                 // the page index
    R0 = SP_DDR_PAGES;
    COMP(R5, R0);
    IF GE JUMP _sp_cb_tagerr;
    R0 = DM(_sp_last);
    COMP(R4, R0);
    IF EQ JUMP _sp_cb_repeat;
    R1 = FEXT R0 BY 20:8;
    R1 = R1 + 1;
    R2 = 255;
    R1 = R1 AND R2;
    R2 = FEXT R4 BY 20:8;
    COMP(R1, R2);
    IF NE JUMP _sp_cb_seqerr;
    R6 = 1;                               // a new page: count it
    JUMP _sp_cb_copy;
_sp_cb_repeat:
    R6 = 0;
_sp_cb_copy:
    R0 = LSHIFT R5 BY 10;                 // page * 1,024 words
    R1 = SP_DDR_BASE_NW32;
    R0 = R0 + R1;
    I1 = R0;
    MODIFY(I0, 4);
    R0 = 1024;
    LCNTR = R0, DO _sp_copy UNTIL LCE;
        R0 = DM(I0, M6);
_sp_copy: DM(I1, M6) = R0 (NW);
    R6 = PASS R6;
    IF EQ JUMP _sp_cb_noted;
    R0 = DM(_sp_pages);
    R0 = R0 + 1;
    DM(_sp_pages) = R0;
    DM(_sp_last) = R4;
_sp_cb_noted:
    R0 = 0;
    DM(_sp_error) = R0;
    R0 = 1;
    DM(_sp_status) = R0;
    JUMP _sp_cb_rearm;
_sp_cb_seqerr:
    R0 = 5;
    JUMP _sp_cb_err;
_sp_cb_tagerr:
    R0 = 4;
_sp_cb_err:
    DM(_sp_error) = R0;
    R0 = DM(_sp_errcount);
    R0 = R0 + 1;
    DM(_sp_errcount) = R0;
    R0 = 2;
    DM(_sp_status) = R0;
_sp_cb_rearm:
    DK_CALL(_sp_arm, _sp_after_rearm);
    JUMP _sp_cb_done;
_sp_cb_bad:
    R0 = 4;
    DM(_sp_error) = R0;
    R0 = 3;
    DM(_sp_status) = R0;
_sp_cb_done:
    DK_RETADDR;
    DK_RESTORE(R5, 3);
    DK_RESTORE(R6, 4);
    DK_RESTORE(R7, 5);
    DK_RESTORE(R0, 6);
    I0 = R0;
    DK_RESTORE(R0, 7);
    I1 = R0;
    DK_RESTORE(R0, 8);
    I2 = R0;
    DK_RESTORE(R0, 9);
    I3 = R0;
    DK_RESTORE(R0, 10);
    I5 = R0;
    DK_RESTORE(R0, 11);
    SP_UNLOCK(R0);
    DK_LEAVE(R3, 2);

.SECTION/DM seg_dmda;
.PKDATA _sp_save, 292;
// _sp_status is the acknowledgement's status and the callback's context word: 0 not armed, 1 armed and idle, 2 armed after an error, 3 dead.
// _sp_error is the error code (1, 2, 3, 8: set-up failures with status 3; 4, 5: a rejected page; 9: the rearm failed). _sp_errcount counts rejected
// pages. _sp_pages counts accepted pages since boot, _sp_last is the last accepted tag. They are the oracles of the host tests as well.
.GLOBAL _sp_started; .VAR _sp_started = 0;
.GLOBAL _sp_status; .VAR _sp_status = 0;
.GLOBAL _sp_error; .VAR _sp_error = 0;
.GLOBAL _sp_errcount; .VAR _sp_errcount = 0;
.GLOBAL _sp_pages; .VAR _sp_pages = 0;
.GLOBAL _sp_last; .VAR _sp_last = 0xd0000000;
.GLOBAL _sp_handle; .VAR _sp_handle = 0;
