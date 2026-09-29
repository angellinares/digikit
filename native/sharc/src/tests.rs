//! Runtime tests (no firmware, no generated code).

use crate::canon;
use crate::mem::Mem;
use crate::rt::bnd;
use crate::rt::*;

fn state() -> Box<St> {
    let mut mem = Mem::new();
    // A loader-backed word at the short-word alias of 0x1000, and one at a
    // raw address.
    mem.load(0x2800_1000, &[1, 2, 3, 4]);
    mem.load(0x0003_0000, &[9, 9, 9, 9]);
    mem.reset();
    let mut s = St::new(mem);
    s.sync_snapshot();
    s
}

#[test]
fn journal_undoes_an_instruction() {
    let mut s = state();
    s.r[3] = V::c(7);
    s.sync_snapshot();
    s.begin();
    s.set_r(3, V::c(8)).unwrap();
    s.set_r(3, V::c(9)).unwrap();
    // The snapshot view still reads the value before the instruction.
    assert_eq!(rv_get(&s, RegView::OLD, 3), V::c(7));
    assert_eq!(rv_get(&s, RegView::CUR, 3), V::c(9));
    s.rollback();
    assert_eq!(s.r[3], V::c(7));
    s.begin();
    s.set_r(3, V::c(10)).unwrap();
    s.commit();
    assert_eq!(rv_get(&s, RegView::OLD, 3), V::c(10));
}

#[test]
fn stacks_and_specials_roll_back() {
    let mut s = state();
    s.begin();
    stk_push_call_stack(&mut s, 0x1234).unwrap();
    stk_push_loops(
        &mut s,
        Loop {
            start_sw: 1,
            end_sw: 2,
            remaining: 3,
            mode: 0,
        },
    )
    .unwrap();
    s_set_special(&mut s, S_MRF, Spec::M(MR::new(MR_MASK, 5))).unwrap();
    s.pc_sw = 99;
    s.rollback();
    assert_eq!(stk_len_call_stack(&s), 0);
    assert_eq!(stk_len_loops(&s), 0);
    assert!(sv_get(&s, SpecView::CUR, S_MRF).is_none());
    assert_eq!(s.pc_sw, 0);
}

#[test]
fn memory_alias_and_explicit_model() {
    let mut s = state();
    // An unbacked low address reads through the short-word alias.
    assert_eq!(
        bnd::_canonical_dm_address(&s, 0x1000, 4, false).unwrap(),
        Some(0x2800_1000)
    );
    let v = bnd::_dm_read(&s, VI::I(0x1000), 4, false).unwrap();
    assert_eq!(v, Some(V::c(0x0403_0201)));
    // A signed byte read sign-extends, then masks to 32 bits.
    s.mem.load(0x2800_2000, &[0x80]);
    s.mem.reset();
    let v = bnd::_dm_read(&s, VI::I(0x2000), 1, true).unwrap();
    assert_eq!(v, Some(V::c(0xFFFF_FF80)));
    // Unwritten internal RAM reads as 0 under the explicit memory model.
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x5000), 4, false).unwrap(),
        Some(V::c(0))
    );
    // A write to an unbacked low address lands at the alias.
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(0x6000), 2, V::c(0xBEEF)).unwrap());
    s.commit();
    assert_eq!(s.mem.byte(0x2800_6000), 0xEF);
    assert!(!s.mem.present(0x6000));
    assert_eq!(
        s.mem.dirty_bytes(),
        vec![(0x2800_6000, 0xEF), (0x2800_6001, 0xBE)]
    );
    // An unknown value is not stored.
    assert!(!bnd::_dm_write(&mut s, VI::I(0x6000), 4, V::UNK).unwrap());
}

#[test]
fn memory_write_rolls_back() {
    let mut s = state();
    s.begin();
    bnd::_dm_write(&mut s, VI::I(0x1000), 1, V::c(0x55)).unwrap();
    assert_eq!(s.mem.byte(0x2800_1000), 0x55);
    s.rollback();
    assert_eq!(s.mem.byte(0x2800_1000), 1);
    assert!(s.mem.dirty_bytes().is_empty());
}

#[test]
fn unmodelled_mmr_traps() {
    let mut s = state();
    s.named_mmrs = vec![0x3100_0000];
    s.set_mmr_windows();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x3100_0000), 4, false),
        Err(TRAP_UNMODELED_MMR)
    );
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(0x3100_0000), 4, V::c(5)).unwrap());
    s.commit();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x3100_0000), 4, false),
        Ok(Some(V::c(5)))
    );
}

#[test]
fn astat_knowledge() {
    let s = state();
    let unk = V::UNK;
    let p = bnd::_astatx_define(&s, unk, 0b101, 0b100);
    assert_eq!((p.m, p.b), (0b101, 0b100));
    assert_eq!(bnd::_astatx_known_bit(&s, p, 2), Some(true));
    assert_eq!(bnd::_astatx_known_bit(&s, p, 1), None);
    let full = bnd::_astatx_define(&s, p, !0b101 & 0xFFFF_FFFF, 0);
    assert!(full.is_c());
    let back = bnd::_astatx_forget(&s, full, 1);
    assert_eq!(back.m, 0xFFFF_FFFE);
    // A known compare shifts CACC.
    let u = FlagUpdate {
        define_mask: 1,
        define_bits: 1,
        forget_mask: 0,
        cacc: 1,
    };
    let r = bnd::_apply_flag_update(&s, V::c(0x0200_0000), u);
    assert_eq!(r, V::c(0x8100_0001));
}

#[test]
fn flag_update_algebra() {
    let s = state();
    let a = bnd::_flags_put(&s, bnd::FLAGS_NONE, 3, Some(true));
    let b = bnd::_flags_put(&s, a, 4, None);
    assert_eq!((b.define_mask, b.define_bits, b.forget_mask), (8, 8, 16));
    let then = bnd::_flags_then(&s, b, bnd::_flags_define(&s, 16, 16));
    assert_eq!(
        (then.define_mask, then.define_bits, then.forget_mask),
        (24, 24, 0)
    );
    let or = bnd::_flags_or(
        &s,
        bnd::_flags_define(&s, 3, 1),
        bnd::_flags_define(&s, 3, 2),
    );
    assert_eq!((or.define_mask, or.define_bits), (3, 3));
}

#[test]
fn mr_words() {
    let s = state();
    let m = bnd::_mr_write_word(&s, Spec::V(V::UNK), 1, V::c(0x8000_0000));
    // MR1 sign-extends into MR2.
    assert_eq!(bnd::_mr_read_word(&s, m, 2), V::c(0xFFFF_FFFF));
    assert!(bnd::_mr_read_word(&s, m, 0).is_unknown());
    let m = bnd::_mr_write_word(&s, m, 0, V::c(7));
    assert_eq!(m.as_mr().signed(), Some(-(1 << 63) + 7));
}

#[test]
fn python_float_rules() {
    let s = state();
    // fcvt-style NaN widening and narrowing.
    let x = bnd::_f32_from_bits(&s, 0x7F80_0001);
    assert_eq!(x.to_bits(), 0x7FF8_0000_2000_0000);
    assert_eq!(bnd::_float32_bits(&s, x), (0x7FC0_0001, false));
    // Overflow of the float32 rounding.
    assert_eq!(bnd::_float32_bits(&s, 1e39), (0x7F80_0000, true));
    // The first NaN operand wins.
    let a = f64::from_bits(0x7FF8_0000_0000_0001);
    let b = f64::from_bits(0x7FF8_0000_0000_0002);
    assert_eq!(fmul(a, b).to_bits(), a.to_bits());
    assert_eq!(fadd(1.0, b).to_bits(), b.to_bits());
    // ldexp rounds once into the subnormal range.
    assert_eq!(scalbn(1.0, -1074), f64::from_bits(1));
    assert_eq!(scalbn(1.5, -1074), f64::from_bits(2));
    assert!(scalbn(1.0, 5000).is_infinite());
}

#[test]
fn python_integer_rules() {
    assert_eq!(floordiv(-7, 2), Ok(-4));
    assert_eq!(pymod(-7, 2), Ok(1));
    assert_eq!(shl(1, 200), Ok(0));
    assert_eq!(shr(-8, 1), Ok(-4));
    assert!(shl(1, -1).is_err());
    assert_eq!(bit_length(255), 8);
}

#[test]
fn canonical_state_round_trip() {
    let mut s = state();
    s.pc_sw = 0x1c4ecf;
    s.r[5] = V::c(0x1234);
    s.r[118] = V::partial(0xF0, 0x30);
    s.special[0] = Spec::M(MR::new(MR_MASK, 42));
    s.special_present = [true; 7];
    s.mmr_put(0x30024, V::c(1));
    s.pending = Some(Pending {
        target: Some(0x1c0000),
        call: true,
        slots: 2,
        return_from_call: false,
        return_sw: Some(-1),
    });
    s.loops
        .push_raw(Loop {
            start_sw: 10,
            end_sw: 20,
            remaining: 3,
            mode: 1,
        })
        .unwrap();
    s.call_stack.push_raw(10).unwrap();
    s.begin();
    bnd::_dm_write(&mut s, VI::I(0x7000), 4, V::c(0xAABBCCDD)).unwrap();
    s.commit();
    let blob = canon::export_state(&s, true);
    let mut t = state();
    canon::import_state(&mut t, &blob).unwrap();
    assert_eq!(canon::export_state(&t, true), blob);
    assert_eq!(t.r[118], V::partial(0xF0, 0x30));
    assert_eq!(t.mem.byte(0x2800_7003), 0xAA);
}

#[test]
fn engine_without_code_traps() {
    let mut e = crate::Engine::new(Mem::new());
    assert_eq!(e.step(1), 0);
    assert!(e.halt.as_deref().unwrap().starts_with("native-trap:"));
}

#[test]
fn snapshot_view_follows_the_journal() {
    let mut s = state();
    s.r[5] = V::c(1);
    s.sync_snapshot();
    s.begin();
    s.set_r(5, V::c(2)).unwrap();
    // _snapshot_uregs mid-instruction: the view is the file as it is now.
    s.snapshot();
    assert_eq!(rv_get(&s, RegView::OLD, 5), V::c(2));
    s.set_r(5, V::c(3)).unwrap();
    assert_eq!(rv_get(&s, RegView::OLD, 5), V::c(2));
    assert_eq!(rv_get(&s, RegView::CUR, 5), V::c(3));
    s.rollback();
    assert_eq!(s.r[5], V::c(1));
    assert_eq!(rv_get(&s, RegView::OLD, 5), V::c(1));
}

#[test]
fn block_register_file_rules() {
    let mut rf = Rf::default();
    rf_put(&mut rf, 4, V::UNK).unwrap();
    assert_eq!(rf.r[4], V::UNK);
    // A value that is not known leaves block code.
    assert_eq!(rf_set(&mut rf, 4, V::UNK), Err(TRAP_BLOCK_UNKNOWN));
    rf_set(&mut rf, 2, V::c(9)).unwrap();
    rf.r[82] = V::c(5);
    rf.o[2] = V::c(8);
    assert_eq!(rf_get(&rf, RegView::CUR, 2), V::c(9));
    assert_eq!(rf_get(&rf, RegView::OLD, 2), V::c(8));
    // PEy's view reads S2 for R2.
    assert_eq!(rf_get(&rf, RegView(RegView::PEY), 2), V::c(5));
}

#[test]
fn plain_ram_fast_paths_follow_the_alias() {
    let mut s = state();
    // 0x1000 has no page of its own: it reads the short-word alias.
    assert_eq!(s.mem.fast_read(0x1000, 4), Some(0x0403_0201));
    assert_eq!(
        bnd::_dm_read_b(&s, VI::I(0x1000), 4, false).unwrap(),
        bnd::_dm_read_full(&s, VI::I(0x1000), 4, false).unwrap()
    );
    // A write lands at the alias (once its bytes are overlay bytes, the
    // fast path takes it too).
    assert!(bnd::_dm_write_nolog_b(&mut s, VI::I(0x1000), 4, V::c(0x0a0b_0c0d)).unwrap());
    assert_eq!(s.mem.read_le(0x2800_1000, 4), 0x0a0b_0c0d);
    assert_eq!(s.mem.fast_write(0x1000, 4, 0x1111_2222), Some(0x0a0b_0c0d));
    assert_eq!(s.mem.read_le(0x2800_1000, 4), 0x1111_2222);
    // No fast path on an MMR page.
    s.named_mmrs = vec![0x2800_1000];
    s.set_mmr_windows();
    assert_eq!(s.mem.fast_read(0x2800_1000, 4), None);
}
