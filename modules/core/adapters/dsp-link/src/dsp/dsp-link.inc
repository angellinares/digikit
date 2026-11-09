// core/dsp-link: the DSP end of the code channel (docs/findings/31-dsp-link.md; plan: docs/plans/digikit-runtime.md, Phase 5).
//
// The ColdFire writes a chunk into the tail of its per-block frame (the data halves of the last frame entries, which no
// stock code reads, finding 23 S4b). This routine runs once per block at the very end of the block handler, reads the chunk
// from the receive ring that handler worked on, checks it and copies the payload into a reserved area of free L2 (_dl_area).
// After the last chunk it invalidates the instruction caches, so that code written to the area can run. It reports progress
// in a 12-byte acknowledgement written into the record the DSP sends back to the ColdFire (offset DL_ACK_OFF).
//
// Where it runs. The hook replaces `DM(<ring toggle>) = R1`, the second last instruction of the block handler's common tail
// (every command path ends there). At that point the handler has built the whole record (the engine clears and refills the
// record each block, so an acknowledgement written earlier would be erased) and is about to store the record's first word:
//   in    I4 = the record pointer (live), R1 = the ring toggle value (live), R2 = the record's first word (live),
//         I6 = the handler's frame: DM(I6 - 3) is the pointer to the receive ring in use, DM(I6 - 2) the record pointer.
//   free  R10, R11, R13, R14, R15, I2, I3, I5: the epilogue reloads them from the frame right after.
//   saved here  R0, R3, R4, R5, R6, and M5, M6, L2, L3 (set to 0, 1, 0, 0 for the routine and put back at the exit: the
//         firmware restores these per DSP context, so their values here are the context's, not constants, finding 45).
//         Not touched: R1, R2, I4, I6, I7, I12, the other M registers. No DO loop is open at the site of a hook that itself
//         runs two (the epilogue's own).
// The displaced instruction runs in the patchkit tail after the final jump.
//
// Wire format of a chunk (little-endian words of the ring; the ColdFire writes the halves, low half first):
//   w0  bits 0-15 DL_MAGIC, bits 16-29 idx (chunk number, 0 for the first of a transfer), bits 30-31 win (the window)
//   w1  bits 0-16 off (destination, in words from the start of the window), bits 17-24 n (payload words),
//       bit 25 LAST, bit 26 END (stop acknowledging), bits 27-31 xid (transfer id, 1..31)
// Windows: 0 is _dl_area (DL_AREA_WORDS long); 1 is the voices registry (_dl_vrows, 24 * _dl_vcount words: a row is 24 words), which the digikit
// runtime fills one word per transfer so that the render scan never sees a half-written row; 2 is the effect rows of core/fx (_dl_frows, 24 *
// _dl_fcount words, all rows of the registry `fx`, free rows included), filled the same way (the pc word last). Windows 1 and 2 refuse every write
// when their registry is not in the image (the externs are 0), window 3 does not exist.
//   w2 ... w(n+1)  the payload
//   w(n+2)  check = s1 XOR s2 over w0 ... w(n+1), where s1 += w and s2 += s1 for each word w (both start at 0)
// A chunk is taken when xid and magic are right, idx equals the number of chunks taken so far (cum) and the check holds.
// idx 0 with a new xid starts a transfer. A chunk with an old idx (resent) or a later idx (one was lost) is ignored.
// A bad check is counted (nerr) and the chunk is dropped; the ColdFire resends from cum. A destination or size outside
// the area stops the transfer for good (status 3) until a new xid starts.
//
// Acknowledgement (three halfwords the ColdFire reads from the record copy, in the order of the ring bytes):
//   a0  bits 0-15 DL_ACK_MAGIC, bits 16-31 cum;   a1  bits 0-7 xid, bits 8-15 status, bits 16-31 nerr
// status: 0 running, 1 done (every chunk taken, last one flagged, caches invalidated), 3 destination out of range.
// It is written in every block while a transfer is active (xid != 0): the engine clears the record each block.
//
// Diagnostics (dl_diag, findings 45, 48 and 77): the fault table. In every block, whatever else is in the image, 32 words go into the record, two in
// each of the slot rows 1 to 16 (slot row r starts at record byte 0x1c + 0x54 r; the words are at +0x44 and +0x48, inside the entries 11 and 12 that
// no DSP code writes and the ColdFire reads only for parameters 36 to 39, the place of the acknowledgement in row 0). Word j (0 to 31) is at
// row 1 + j / 2, +0x44 for even j, +0x48 for odd j; as a record word index: 7 + 21 (1 + j / 2) + 17 + (j & 1). Rows 13 to 16 hold the words of the
// fault trace; the record has 32 rows, so no row is added. On the Digitone II these bytes are samples of the effect buses (finding 31): rows 13 to 16 add
// four more rows to the twelve that the table used before.
//   0 DL_DG_MAGIC (low half 0xD1A6) and the word count 32 (high half)     1 blocks that ran this hook (the heartbeat)
//   2 blocks that reached core/dsp-voice's hook (its heartbeat)            3 M5     4 M6     5 M7     6 M13     7 M14     8 M15
//   9 I6     10 I7     11 B7     12 L7     13 MODE1  (registers as the firmware left them at the first voice of the block)
//   14 the voice site's `last`, the breadcrumb: stage | voice << 8 | machine type << 16 (stages: dsp-voice.s, _dv_crumb; the Digitakt II wrapper also writes it, with the
//      header and words 15 and 16, straight into both reply records, so that it is there when the core stops inside a routine; the Digitone II wrapper writes
//      only stage 4 (the routine is about to be called) and 5 (it returned), with the slot as the voice)
//   15 dispatches to registered machine types since the start (counted before the routine is called)
//   16 routines that returned (15 - 16 is 1 while a routine runs; a core stopped inside one leaves it at 1)
//   17 the guard's fault count (low 16 bits) | kind of the last fault << 16 (1 Inf or NaN, 2 over the level bound) | types it disabled << 24
//   18 type | voice << 16 of the last fault   19 to 22 the 8 parameter words of that call, in pairs (knob 2k | knob 2k + 1 << 16)
//   23 the machine type the guard disabled last (docs/findings/51)
//   24 the effect site's `last` (core/fx, fx.s _fx_mark): stage (4 the core is about to call the module, 5 it returned) | site << 8 (2 master route, 3 send route) |
//      the row's byte offset in the effect table << 16 (96 bytes a row); written straight into both reply records at the call and the return
//   25 the module step: the number the module stores into `_dk_step` (the import core/dyndsp exports). The sites (core/fx, core/dsp-voice) clear `_dk_step` right
//      before each call into a module; the block tail copies it into this block's record, after all module calls of the block, and clears it; a block in which the
//      last module stored nothing carries 0
//   26 module calls of the master route   27 module calls of the send route (core/fx)
//   28 effect faults (low 16 bits) | the row of the last fault (byte offset + 1) << 16   31 the row switched off last (byte offset + 1)
//   29 the address of this block's reply record   30 the address of the one before (the two rings)
// The words 2 to 23 are filled when core/dsp-voice is in the image, 24 and 26 to 28 and 31 when core/fx is; 0, 1, 29 and 30 always. `last` is written by
// the sites themselves into both records (a core that stopped never reaches this hook); the block tail copies what the sites keep in
// DM (`_dv_diag`, `_fx_diag`) and the static `_dk_step`.
// The block tail also stores this block's record address into the sites' tables (_dv_rec, _fx_rec; [0] this block's, [1] the one before) and, as [2], the address of `_dk_step`.
// The ColdFire side is dl_diag(out, n) in dsp-link.c.

#define DL_MAGIC 0xD15C
#define DL_ACK_MAGIC 0xA5D1
#define DL_DG_MAGIC 0xD1A6
#define DL_ACK_OFF 0x60
#define DL_SHL1C_CFG 0x3E000

.SECTION/PM seg_pmco;
.GLOBAL _dl_hook;
.EXTERN _dl_back;
#ifdef DL_CHAIN_SP
.EXTERN _sp_tail;
#endif
.EXTERN _dl_area;
.EXTERN _dl_vrows;
.EXTERN _dl_vcount;
.EXTERN _dl_frows;
.EXTERN _dl_fcount;
.EXTERN _dv_diag;
.EXTERN _dv_rec;
.EXTERN _fx_diag;
.EXTERN _fx_rec;

_dl_hook:
    DM(_dl_sv0) = R0;
    DM(_dl_sv3) = R3;
    DM(_dl_sv4) = R4;
    DM(_dl_sv5) = R5;
    DM(_dl_sv6) = R6;
    R0 = M5;
    DM(_dl_svm5) = R0;
    R0 = M6;
    DM(_dl_svm6) = R0;
    R0 = L2;
    DM(_dl_svl2) = R0;
    R0 = L3;
    DM(_dl_svl3) = R0;
    M5 = 0;
    M6 = 1;
    L2 = 0;
    L3 = 0;
    R0 = DM(-3, I6);                      // the receive ring in use
    R3 = DL_TAIL_OFF;
    R0 = R3 + R0;
    I3 = R0;
    R10 = DM(I3, M6);                     // w0
    R3 = 0xffff;
    R4 = R10 AND R3;
    R3 = DL_MAGIC;
    COMP(R4, R3);
    IF NE JUMP _dl_ack;
    R13 = FEXT R10 BY 16:14;              // idx
    R14 = DM(I3, M6);                     // w1
    R4 = FEXT R14 BY 27:5;                // xid
    R4 = PASS R4;
    IF EQ JUMP _dl_ack;
    R3 = DM(_dl_xid);
    COMP(R4, R3);
    IF EQ JUMP _dl_same;
    // another transfer: only its first chunk (idx 0) starts it
    R5 = 0x4000000;
    R5 = R14 AND R5;                      // END of a transfer that is not the current one: nothing to do
    IF NE JUMP _dl_ack;
    R13 = PASS R13;
    IF NE JUMP _dl_ack;
    DM(_dl_xid) = R4;
    R3 = 0;
    DM(_dl_cum) = R3;
    DM(_dl_status) = R3;
    DM(_dl_nerr) = R3;
_dl_same:
    R5 = 0x4000000;
    R5 = R14 AND R5;
    IF EQ JUMP _dl_notend;
    R3 = 0;
    DM(_dl_xid) = R3;                     // END: stop acknowledging
    JUMP _dl_out;
_dl_notend:
    R3 = DM(_dl_status);
    R5 = 2;
    COMP(R3, R5);
    IF GE JUMP _dl_ack;                   // a stopped transfer stays stopped
    R3 = DM(_dl_cum);
    COMP(R13, R3);
    IF NE JUMP _dl_ack;                   // old (resent) or later (lost) chunk
    // n and off, and the limits
    R5 = FEXT R14 BY 17:8;                // n (zero extended)
    R6 = FEXT R14 BY 0:17;                // off
    R3 = DL_TAIL_WORDS - 3;
    COMP(R5, R3);
    IF GT JUMP _dl_range;
    R0 = FEXT R10 BY 30:2;                // the window
    R0 = PASS R0;
    IF NE JUMP _dl_win1;
    R3 = DL_AREA_WORDS;                   // window 0: the area
    R4 = _dl_area;
    JUMP _dl_wdone;
_dl_win1:
    R3 = 1;
    COMP(R0, R3);
    IF NE JUMP _dl_win2;
    R3 = _dl_vcount;                      // window 1: the voice rows (24 words each, the terminator row excluded)
    R4 = LSHIFT R3 BY 4;
    R3 = LSHIFT R3 BY 3;
    R3 = R3 + R4;                         // count * 24
    R4 = _dl_vrows;
    JUMP _dl_wdone;
_dl_win2:
    R3 = 2;
    COMP(R0, R3);
    IF NE JUMP _dl_range;                 // window 3 does not exist
    R3 = _dl_fcount;                      // window 2: the effect rows (24 words each, the terminator row excluded)
    R4 = LSHIFT R3 BY 4;
    R3 = LSHIFT R3 BY 3;
    R3 = R3 + R4;                         // count * 24
    R4 = _dl_frows;
_dl_wdone:
    DM(_dl_wbase) = R4;
    R4 = R5 + R6;
    COMP(R4, R3);
    IF GT JUMP _dl_range;
    // the check over w0, w1 and the payload
    R3 = R10;                             // s1
    R4 = R10;                             // s2
    R3 = R14 + R3;
    R4 = R3 + R4;
    R5 = PASS R5;
    IF EQ JUMP _dl_sumdone;
    R15 = I3;                             // payload start
    LCNTR = R5, DO _dl_suml UNTIL LCE;
        R0 = DM(I3, M6);
        R3 = R0 + R3;
_dl_suml: R4 = R3 + R4;
    I3 = R15;
_dl_sumdone:
    R3 = R4 XOR R3;
    // I3 = payload start; the check word follows the payload
    R0 = LSHIFT R5 BY 2;                  // 4 n bytes
    R0 = R15 + R0;
    I2 = R0;
    R0 = DM(I2, M5);
    COMP(R0, R3);
    IF EQ JUMP _dl_good;
    R3 = DM(_dl_nerr);
    R3 = R3 + 1;
    DM(_dl_nerr) = R3;
    JUMP _dl_ack;
_dl_good:
    // copy the payload to the window base + 4 off
    R0 = LSHIFT R6 BY 2;
    R3 = DM(_dl_wbase);
    R0 = R3 + R0;
    I2 = R0;
    R5 = PASS R5;
    IF EQ JUMP _dl_copied;
    LCNTR = R5, DO _dl_copyl UNTIL LCE;
        R0 = DM(I3, M6);
_dl_copyl: DM(I2, M6) = R0;
_dl_copied:
    R3 = DM(_dl_cum);
    R3 = R3 + 1;
    DM(_dl_cum) = R3;
    R5 = 0x2000000;
    R5 = R14 AND R5;                      // LAST
    IF EQ JUMP _dl_ack;
    // The code is in place. The copy went through the data cache (write allocate, write back), so first write the dirty lines back to
    // memory: a write-back invalidation of the DM and PM caches together, WB and INV set at the same time (sc58x-2158x-prm p. 8-11).
    // It stalls the core until it is done. Then drop what the instruction caches hold: FLUSH CACHE clears the instruction-conflict
    // cache (p. 4-50), the ICAINV bit of SHL1C_CFG invalidates the L1 instruction cache (p. 8-12, ch. 33). Invalidation and
    // write-back invalidation are not mixed (p. 8-11), and twelve 48-bit instructions follow every cache MMR write (p. 8-9).
    // The operation bits (ICAINV 6, DMCAINV 14, DMCAWB 15, PMCAINV 22, PMCAWB 23) are cleared from the value read: the stock start-up
    // writes the INV bits together with the enables (0x414141), and the manual does not say whether they clear themselves or read back
    // as 1. With C = the register without them, each operation is a write of C with its own bits set, and C is written between them
    // (a 0 to 1 change for an edge trigger, no stale bit for a level trigger). Correct for both.
    .NOCOMPRESS;
    FLUSH CACHE;
    R0 = DM(DL_SHL1C_CFG);
    R3 = 0xFF3F3FBF;
    R0 = R0 AND R3;                       // C
    R3 = 0xC0C000;                        // DMCAWB, DMCAINV, PMCAWB, PMCAINV
    R3 = R0 OR R3;
    DM(DL_SHL1C_CFG) = R3;                // write-back invalidation of the DM and PM caches
    NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP;
    DM(DL_SHL1C_CFG) = R0;                // C
    NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP;
    R3 = BSET R0 BY 6;                    // ICAINV
    DM(DL_SHL1C_CFG) = R3;
    LCNTR = 40, DO _dl_wait UNTIL LCE;    // the invalidation takes about 32 cycles; keep the bit set that long
        NOP;
_dl_wait: NOP;
    DM(DL_SHL1C_CFG) = R0;                // C
    NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP; NOP;
    .COMPRESS;
    R3 = 1;
    DM(_dl_status) = R3;
    JUMP _dl_ack;
_dl_range:
    R3 = 3;
    DM(_dl_status) = R3;
_dl_ack:
    R3 = DM(_dl_xid);
    R3 = PASS R3;
    IF EQ JUMP _dl_out;
    R4 = I4;                              // the record pointer
    R5 = DL_ACK_OFF;
    R4 = R5 + R4;
    I2 = R4;
    R4 = DM(_dl_cum);
    R4 = LSHIFT R4 BY 16;
    R5 = DL_ACK_MAGIC;
    R4 = R5 OR R4;
    DM(I2, M6) = R4;                      // a0
    R4 = DM(_dl_status);
    R4 = LSHIFT R4 BY 8;
    R4 = R3 OR R4;
    R5 = DM(_dl_nerr);
    R5 = LSHIFT R5 BY 16;
    R4 = R5 OR R4;
    DM(I2, M6) = R4;                      // a1
_dl_out:
    R3 = I4;                              // the record pointer
    I2 = R3;
    R0 = DM(_dl_beat);
    R0 = R0 + 1;
    DM(_dl_beat) = R0;
    DM(46, I2) = R0;                      // word 1
    R0 = 0x20D1A6;                        // DL_DG_MAGIC | 32 << 16
    DM(45, I2) = R0;                      // word 0
    R3 = _dv_diag;                        // 0 when core/dsp-voice is not in the image
    R3 = PASS R3;
    IF EQ JUMP _dl_novd;
    I3 = R3;
    R0 = DM(0, I3);
    DM(66, I2) = R0;                      // word 2
    R0 = DM(1, I3);
    DM(67, I2) = R0;                      // word 3
    R0 = DM(2, I3);
    DM(87, I2) = R0;                      // word 4
    R0 = DM(3, I3);
    DM(88, I2) = R0;                      // word 5
    R0 = DM(4, I3);
    DM(108, I2) = R0;                      // word 6
    R0 = DM(5, I3);
    DM(109, I2) = R0;                      // word 7
    R0 = DM(6, I3);
    DM(129, I2) = R0;                      // word 8
    R0 = DM(7, I3);
    DM(130, I2) = R0;                      // word 9
    R0 = DM(8, I3);
    DM(150, I2) = R0;                      // word 10
    R0 = DM(9, I3);
    DM(151, I2) = R0;                      // word 11
    R0 = DM(10, I3);
    DM(171, I2) = R0;                      // word 12
    R0 = DM(11, I3);
    DM(172, I2) = R0;                      // word 13
    R0 = DM(12, I3);
    DM(192, I2) = R0;                      // word 14: the last breadcrumb
    R0 = DM(13, I3);
    DM(193, I2) = R0;                      // word 15: dispatches to registered types
    R0 = DM(14, I3);
    DM(213, I2) = R0;                      // word 16: routines that returned
    R0 = DM(15, I3);
    DM(214, I2) = R0;                      // word 17: guard faults (16 bits) | last kind << 16 | disabled types << 24
    R0 = DM(16, I3);
    DM(234, I2) = R0;                      // word 18: type | voice << 16 of the last fault
    R0 = DM(17, I3);
    DM(235, I2) = R0;                      // word 19: the 8 parameter words of the faulting call, pairs: knob 0 | knob 1 << 16
    R0 = DM(18, I3);
    DM(255, I2) = R0;                      // word 20
    R0 = DM(19, I3);
    DM(256, I2) = R0;                      // word 21
    R0 = DM(20, I3);
    DM(276, I2) = R0;                      // word 22
    R0 = DM(21, I3);
    DM(277, I2) = R0;                      // word 23: the type the guard disabled last
    R3 = _dv_rec;                         // the two reply records, for core/dsp-voice's breadcrumbs: [0] this block's, [1] the one before
    I3 = R3;
    R0 = DM(0, I3);
    DM(1, I3) = R0;
    R0 = I4;
    DM(0, I3) = R0;
    R0 = _dk_step;                        // the module step word, for the site's clear before a module call
    DM(2, I3) = R0;
_dl_novd:
    R3 = _fx_diag;                        // 0 when core/fx is not in the image
    R3 = PASS R3;
    IF EQ JUMP _dl_nofx;
    I3 = R3;
    R0 = DM(0, I3);
    DM(297, I2) = R0;                     // word 24: core/fx's `last`
    R0 = DM(4, I3);
    DM(318, I2) = R0;                     // word 26: module calls of the master route
    R0 = DM(5, I3);
    DM(319, I2) = R0;                     // word 27: module calls of the send route
    R0 = DM(1, I3);
    R4 = 0xffff;
    R0 = R0 AND R4;
    R5 = DM(2, I3);
    R5 = LSHIFT R5 BY 16;
    R0 = R0 OR R5;
    DM(339, I2) = R0;                     // word 28: faults (16 bits) | row of the last fault (byte offset + 1) << 16
    R0 = DM(3, I3);
    DM(361, I2) = R0;                     // word 31: the row switched off last (byte offset + 1)
    R3 = _fx_rec;
    I3 = R3;
    R0 = DM(0, I3);
    DM(1, I3) = R0;
    R0 = I4;
    DM(0, I3) = R0;
    R0 = _dk_step;
    DM(2, I3) = R0;
_dl_nofx:
    R0 = DM(_dk_step);                    // the module step: the last module call of the block stored it (the sites clear it before each call)
    DM(298, I2) = R0;                     // word 25
    R3 = 0;
    DM(_dk_step) = R3;                    // a block without a module call carries 0
    R0 = DM(_dk_rec);
    DM(360, I2) = R0;                     // word 30: the record of the block before
    R0 = I4;
    DM(340, I2) = R0;                     // word 29: this block's record
    DM(_dk_rec) = R0;
_dl_nodiag:
    R0 = DM(_dl_svm5);
    M5 = R0;
    R0 = DM(_dl_svm6);
    M6 = R0;
    R0 = DM(_dl_svl2);
    L2 = R0;
    R0 = DM(_dl_svl3);
    L3 = R0;
    R0 = DM(_dl_sv0);
    R3 = DM(_dl_sv3);
    R4 = DM(_dl_sv4);
    R5 = DM(_dl_sv5);
    R6 = DM(_dl_sv6);
#ifdef DL_CHAIN_SP
    // core/sample-pool (DN2) hangs its own block work on this tail: _sp_tail saves every register itself and ends in the same
    // `_dl_back` (it is 0 when that module is not in the image). R10 is free here, the epilogue reloads it from the frame.
    R10 = _sp_tail;
    R10 = PASS R10;
    IF EQ JUMP _dl_back;
    JUMP _sp_tail;
#else
    JUMP _dl_back;
#endif

.SECTION/DM seg_dmda;
.GLOBAL _dl_xid;
.GLOBAL _dl_cum;
.GLOBAL _dl_status;
.GLOBAL _dl_nerr;
.GLOBAL _dk_step;
.VAR _dl_xid = 0;
.VAR _dl_cum = 0;
.VAR _dl_status = 0;
.VAR _dl_nerr = 0;
.VAR _dl_wbase = 0;
.VAR _dl_sv0 = 0;
.VAR _dl_sv3 = 0;
.VAR _dl_sv4 = 0;
.VAR _dl_sv5 = 0;
.VAR _dl_sv6 = 0;
.VAR _dl_svm5 = 0;
.VAR _dl_svm6 = 0;
.VAR _dl_svl2 = 0;
.VAR _dl_svl3 = 0;
.VAR _dl_beat = 0;
// The module step (fault table word 25): the static word that core/dyndsp exports as `_dk_step`.
.VAR _dk_step = 0;
// The previous block's record address (word 30).
.VAR _dk_rec = 0;
