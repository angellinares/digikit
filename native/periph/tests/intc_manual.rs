//! Unit tests against MCF5441XRM chapter 17 (Interrupt Controller Modules),
//! independent of any trace.
//!
//! `oracle`-prefixed tests check [`IntcBank::read`]/[`write`]/
//! [`level_for_vector`], the behaviour the replay harness uses (matches the
//! Python emulator: every register here is plain RAM except the masking
//! computation -- see `intc.rs`'s module docs). `hw_`-prefixed tests check
//! the hardware-accurate extras the frozen oracle does not implement
//! (INTFRC, SIMR/CIMR, IACK) -- not used by the replay, per the plan's
//! instruction to implement an unmodelled register from the manual and mark
//! it oracle-exempt.

use periph::intc::{BASES, IntcBank, VECTOR_BASE};

#[test]
fn vector_numbering_section_17_3_1_3() {
    // vector = 64*index + source, for INTC0/1/2.
    assert_eq!(VECTOR_BASE, [64, 128, 192]);
    assert_eq!(BASES, [0xFC048000, 0xFC04C000, 0xFC050000]);
}

#[test]
fn oracle_registers_round_trip_as_plain_ram() {
    // The Python emulator never intercepts a guest access here (see
    // `emu.pit.interrupt_level`, which only *reads* IMR/ICR); IPR, INTFRC,
    // SIMR and CIMR all round-trip exactly like any other unmodelled
    // register.
    let mut intc = IntcBank::new();
    for off in [0x00u32, 0x04, 0x10, 0x14, 0x1C, 0x1D] {
        intc.write(BASES[0] + off, 4, 0xDEAD_BEEF);
    }
    assert_eq!(intc.read(BASES[0], 4), Some(0xDEAD_BEEF)); // IPRH
    assert_eq!(intc.read(BASES[0] + 0x04, 4), Some(0xDEAD_BEEF)); // IPRL
    assert_eq!(intc.read(BASES[0] + 0x10, 4), Some(0xDEAD_BEEF)); // INTFRCH
    assert_eq!(intc.read(BASES[0] + 0x14, 4), Some(0xDEAD_BEEF)); // INTFRCL
}

#[test]
fn imr_resets_all_masked() {
    // RM SS17.2.2: "The IMRn is set to all ones by reset, disabling all
    // interrupt requests."
    let intc = IntcBank::new();
    // ICR must be nonzero to distinguish "masked" from "level 0 disabled";
    // set every source's ICR to level 1 first via a fresh bank with a write.
    let mut intc2 = IntcBank::new();
    for src in 0u32..64 {
        intc2.write(BASES[0] + 0x40 + src, 1, 1);
    }
    for vec in 64u16..128 {
        assert_eq!(
            intc2.level_for_vector(vec),
            None,
            "vector {vec} masked at reset"
        );
    }
    // Sanity: fresh (unwritten) ICR bank also refuses (level 0, disabled).
    assert_eq!(intc.level_for_vector(64), None);
}

#[test]
fn oracle_zeroed_bank_differs_from_hardware_reset_only_at_construction() {
    let reset = IntcBank::new();
    let oracle = IntcBank::oracle_zeroed();
    for base in BASES {
        assert_eq!(reset.read(base + 0x08, 4), Some(0xFFFF_FFFF));
        assert_eq!(reset.read(base + 0x0c, 4), Some(0xFFFF_FFFF));
        assert_eq!(oracle.read(base + 0x08, 4), Some(0));
        assert_eq!(oracle.read(base + 0x0c, 4), Some(0));
    }
}

#[test]
fn icr_level_zero_disables_regardless_of_mask() {
    let mut intc = IntcBank::new();
    // Unmask source 13 (PIT0, INTC2) but leave ICR at reset (0).
    let cur = intc.read(BASES[2] + 0x0C, 4).unwrap();
    intc.write(BASES[2] + 0x0C, 4, cur & !(1 << 13));
    assert_eq!(
        intc.level_for_vector(205),
        None,
        "ICR level 0 disables regardless of mask"
    );
}

#[test]
fn level_for_vector_matches_emu_pit_interrupt_level() {
    // `emu.pit.interrupt_level`: `icr = ... & 0x07`; masked = IMR bit; ->
    // None if level 0 or masked, else the level.
    let mut intc = IntcBank::new();
    intc.write(BASES[2] + 0x40 + 13, 1, 5); // PIT0 (INTC2 src 13), level 5
    let cur = intc.read(BASES[2] + 0x0C, 4).unwrap();
    intc.write(BASES[2] + 0x0C, 4, cur & !(1 << 13)); // unmask
    assert_eq!(intc.level_for_vector(205), Some(5));
    // Re-mask: back to None.
    let cur = intc.read(BASES[2] + 0x0C, 4).unwrap();
    intc.write(BASES[2] + 0x0C, 4, cur | (1 << 13));
    assert_eq!(intc.level_for_vector(205), None);
}

#[test]
fn icr_high_bits_are_stored_verbatim_reserved_bits_not_enforced() {
    // RM: ICR bits 7-3 "reserved, must be cleared" -- but that is a firmware
    // obligation, not something the register itself enforces (the Python
    // oracle does not either: `interrupt_level` masks with `& 0x07` in its
    // own code, the stored byte is untouched). `level_for_vector` must still
    // report the correct 3-bit level even if a caller wrote extra bits.
    let mut intc = IntcBank::new();
    intc.write(BASES[0] + 0x40 + 5, 1, 0xF8 | 4); // reserved bits set + level 4
    let cur = intc.read(BASES[0] + 0x0C, 4).unwrap();
    intc.write(BASES[0] + 0x0C, 4, cur & !(1 << 5));
    assert_eq!(intc.level_for_vector(69), Some(4));
    assert_eq!(intc.read(BASES[0] + 0x40 + 5, 1), Some(0xF8 | 4));
}

// -- hardware-accurate extras (manual-only; oracle-exempt) -------------------

#[test]
fn hw_simr_sets_imr_bits() {
    // RM SS17.2.5 (p.340): SIMR sets the corresponding IMR bit; SALL (bit 6)
    // sets the whole register.
    let mut intc = IntcBank::new();
    // Clear everything first via CIMR CALL, to start from a known state.
    intc.hw_apply_simr_cimr(BASES[0] + 0x1D, 0x40); // CIMR, CALL
    assert_eq!(intc.read(BASES[0] + 0x0C, 4), Some(0));
    intc.hw_apply_simr_cimr(BASES[0] + 0x1C, 5); // SIMR, bit 5
    assert_eq!(intc.read(BASES[0] + 0x0C, 4), Some(1 << 5));
    intc.hw_apply_simr_cimr(BASES[0] + 0x1C, 0x40); // SALL
    assert_eq!(intc.read(BASES[0] + 0x0C, 4), Some(0xFFFF_FFFF));
}

#[test]
fn hw_cimr_clears_imr_bits() {
    // RM SS17.2.6 (p.341).
    let mut intc = IntcBank::new(); // reset: all masked
    intc.hw_apply_simr_cimr(BASES[0] + 0x1D, 3); // CIMR, clear bit 3
    let imr = intc.read(BASES[0] + 0x0C, 4).unwrap();
    assert_eq!(imr & (1 << 3), 0);
    assert_eq!(imr, !(1 << 3));
}

#[test]
fn hw_intfrc_asserts_ipr_independent_of_mask() {
    // RM SS17.2.3 (p.339): "The assertion of an interrupt request via the
    // interrupt force register is not affected by the interrupt mask
    // register." Finding: "the INTC lane should model INTFRC" -- the
    // Python oracle never delivers from it (see pit.rs's docs on PIT0's
    // bit-13 self-force), so this is exercised only here.
    let mut intc = IntcBank::new(); // IMR all masked (reset)
    intc.write(BASES[2] + 0x14, 4, 1 << 13); // INTFRCL bit 13 (PIT0's source)
    let (iprl, _) = intc.hw_computed_ipr(2, 0, 0);
    assert_ne!(
        iprl & (1 << 13),
        0,
        "IPR must show the forced source regardless of IMR"
    );
}

#[test]
fn hw_iack_returns_highest_priority_unmasked_source_at_a_level() {
    // RM SS17.2.10 (p.351-352): a level-n IACK returns the highest source
    // number pending and unmasked at that level; Table 17-19's own example
    // (sources 40, 22, 8, 2 all at one level -> 40 first).
    let mut intc = IntcBank::new();
    for src in [2u32, 8, 22, 40] {
        intc.write(BASES[0] + 0x40 + src, 1, 3); // level 3
        let imr_addr = if src < 32 {
            BASES[0] + 0x0C
        } else {
            BASES[0] + 0x08
        };
        let cur = intc.read(imr_addr, 4).unwrap();
        intc.write(imr_addr, 4, cur & !(1 << (src % 32)));
    }
    let asserted = (1u32 << 2) | (1 << 8) | (1 << 22) | (1 << (40 - 32)); // src 40 is in IPRH
    // Split low/high for the two force/assert inputs; sources < 32 are low.
    let asserted_low = (1u32 << 2) | (1 << 8) | (1 << 22);
    let asserted_high = 1u32 << (40 - 32);
    let _ = asserted; // (kept for documentation of the combined view)
    let vec = intc.hw_iack(0, 3, asserted_low, asserted_high);
    assert_eq!(vec, (VECTOR_BASE[0] + 40) as u8, "source 40 has priority");
}

#[test]
fn hw_iack_spurious_when_nothing_pending_at_that_level() {
    let intc = IntcBank::new();
    assert_eq!(
        intc.hw_iack(0, 3, 0, 0),
        0x18,
        "RM SS17.2.10: spurious vector is 0x18"
    );
}
