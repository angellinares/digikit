//! Runtime tests (no firmware, no generated code).

use crate::canon;
use crate::mem::Mem;
use crate::rt::bnd;
use crate::rt::*;

#[test]
fn aconv_preserves_known_destination_address_spaces() {
    let s = St::new(Mem::new());
    for (word, byte) in [
        (0x90080, 0x240200),
        (0xb0040, 0x2c0100),
        (0x08000020, 0x20000080),
        (0x10000020, 0x80000080),
    ] {
        assert_eq!(bnd::_aconv(&s, V::c(word), true, 0, 0).unwrap(), V::c(byte));
        assert_eq!(bnd::_aconv(&s, V::c(byte), true, 0, 0).unwrap(), V::c(byte));
        assert_eq!(
            bnd::_aconv(&s, V::c(byte), false, 0, 0).unwrap(),
            V::c(word)
        );
        assert_eq!(
            bnd::_aconv(&s, V::c(word), false, 0, 0).unwrap(),
            V::c(word)
        );
    }
}

#[test]
fn isa_vector_branch_decodes_public_absolute_target() {
    // Public Type8a fields: IF TRUE JUMP 0x123456 (DB), fixed ISA word.
    let raw = 0x0600_0000_0000 | (31 << 33) | (1 << 26) | 0x123456;
    let insn = crate::decode::decode_isa(raw);
    assert_eq!(insn.type_name, "8a_abs");
    assert_eq!(insn.length_bytes, Some(6));
    assert_eq!(
        insn.fields()
            .iter()
            .find(|field| field.key == "addr[23:16]")
            .map(|field| field.value),
        Some(0x12)
    );
    assert_eq!(
        insn.fields()
            .iter()
            .find(|field| field.key == "addr[15:0]")
            .map(|field| field.value),
        Some(0x3456)
    );
}

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
fn software_interrupt_probe_respects_masks_and_nesting() {
    let mut engine = crate::Engine::new(Mem::new());
    engine.s.r[114] = V::c((1 << 12) | (1 << 11));
    engine.s.r[122] = V::c((1 << 31) | (1 << 28));
    engine.s.r[123] = V::c(0xf000_0000);
    engine.s.r[124] = V::c(0);
    assert_eq!(engine.software_interrupt_candidate(), Some(28));
    engine.s.r[124] = V::c(1 << 29);
    assert_eq!(engine.software_interrupt_candidate(), Some(28));
    engine.s.r[122] = V::c(1 << 31);
    assert_eq!(engine.software_interrupt_candidate(), None);
    engine.s.r[124] = V::c(0);
    engine.s.r[114] = V::c(0);
    assert_eq!(engine.software_interrupt_candidate(), None);
    engine.s.r[114] = V::c(1 << 12);
    engine.s.r[123] = V::c(0);
    assert_eq!(engine.software_interrupt_candidate(), None);
}

#[test]
fn software_interrupt_probe_stops_before_an_instruction_and_defers_delay_slots() {
    let mut engine = crate::Engine::new(Mem::new());
    engine.s.r[114] = V::c(1 << 12);
    engine.s.r[122] = V::c(1 << 31);
    engine.s.r[123] = V::c(1 << 31);
    engine.s.r[124] = V::c(0);
    engine.s.pending = Some(Pending {
        target: Some(0x40),
        call: false,
        slots: 2,
        return_from_call: false,
        return_sw: None,
    });
    assert_eq!(engine.software_interrupt_candidate(), None);
    engine.s.pending = None;
    assert!(!engine.stop_software_interrupt);
    assert_eq!(engine.set_option(7, 1), 0);
    assert_eq!(engine.step(1), 0);
    assert_eq!(engine.s.icount, 0);
    assert!(engine.halt.as_deref().unwrap().starts_with("diagnostic:"));
    assert!(engine.s.call_stack.items().is_empty());
    assert!(engine.s.status_stack.items().is_empty());
}

#[test]
fn alternate_banks_apply_after_one_completed_following_instruction() {
    let mut s = state();
    s.cfg.bank_model = true;
    s.cfg.refresh();
    s.r[0] = V::c(1);
    s.r[16] = V::c(16);
    s.r[80] = V::c(80);
    s.bank_alt[0] = V::c(101);
    s.bank_alt[16] = V::c(116);
    s.bank_alt[80] = V::c(180);
    s.bank_requested_mask = 1 << 10;
    s.commit(); // MODE1 write: latch only.
    assert_eq!(s.r[0], V::c(1));
    s.bank_requested_mask = 1 << 4;
    s.commit(); // following instruction: SRRFL takes effect.
    assert_eq!(s.r[0], V::c(101));
    assert_eq!(s.r[80], V::c(180));
    assert_eq!(s.r[16], V::c(16));
    s.commit(); // following request: SRD1L takes effect.
    assert_eq!(s.r[0], V::c(1));
    assert_eq!(s.r[16], V::c(116));
}

#[test]
fn trapped_mode1_bank_request_rolls_back() {
    let mut s = state();
    s.cfg.bank_model = true;
    s.cfg.refresh();
    s.bank_requested_mask = -1;
    s.begin();
    bnd::_bank_request(&mut s, V::c(1 << 10)).unwrap();
    assert_eq!(s.bank_requested_mask, 1 << 10);
    s.rollback();
    assert_eq!(s.bank_requested_mask, -1);
}

#[test]
fn canonical_v2_preserves_bank_state() {
    let mut source = state();
    source.cfg.bank_model = true;
    source.cfg.refresh();
    source.bank_active_mask = 1 << 10;
    source.bank_pending_mask = 1 << 4;
    source.bank_requested_mask = 1 << 3;
    source.bank_alt[0] = V::c(42);
    let blob = canon::export_state(&source, false);
    let mut restored = state();
    canon::import_state(&mut restored, &blob).unwrap();
    assert!(restored.cfg.bank_model);
    assert_eq!(restored.bank_active_mask, 1 << 10);
    assert_eq!(restored.bank_pending_mask, 1 << 4);
    assert_eq!(restored.bank_requested_mask, 1 << 3);
    assert_eq!(restored.bank_alt[0], V::c(42));
}

#[test]
fn physical_stack_wire_state_and_pointer_requests_survive_rollback() {
    let mut source = state();
    source.cfg.stack_model = true;
    source.cfg.refresh();
    source.pc_stack.push_raw(0x0100_0123).unwrap();
    source.pc_stack.push_raw(0x0100_0456).unwrap();
    source.pc_stack_pending = 1;
    source.begin();
    bnd::_pc_stack_request(&mut source, V::c(0)).unwrap();
    stk_set_pc_stack(&mut source, 1, 0x0100_0789).unwrap();
    source.rollback();
    assert_eq!(source.pc_stack_requested, -1);
    assert_eq!(source.pc_stack.items(), &[0x0100_0123, 0x0100_0456]);
    let blob = canon::export_state(&source, false);
    let mut restored = state();
    canon::import_state(&mut restored, &blob).unwrap();
    assert!(restored.cfg.stack_model);
    assert!(!restored.cfg.block_ok);
    assert_eq!(restored.pc_stack_pending, 1);
    restored.begin();
    restored.commit();
    assert_eq!(restored.pc_stack.items(), &[0x0100_0123]);
    assert_eq!(restored.r[100], V::c(0x0100_0123));
    assert_eq!(restored.r[101], V::c(1));
}

#[test]
fn diagnostic_pc_breakpoint_stops_before_clock_or_instruction_effects() {
    let mut engine = crate::Engine::new(Mem::new());
    assert_eq!(engine.set_option(8, -2), -1);
    assert_eq!(engine.set_option(8, 0x0100_0000), -1);
    assert_eq!(engine.set_option(8, 0), 0);
    assert_eq!(engine.set_option(5, 1), 0);
    let clock = engine.s.r[105];
    assert_eq!(engine.step(1), 0);
    assert_eq!(engine.halt.as_deref(), Some("diagnostic: PC breakpoint"));
    assert_eq!(engine.s.r[105], clock);
    assert_eq!(engine.s.icount, 0);
    assert_eq!(engine.set_option(8, -1), 0);
    assert_eq!(engine.stop_pc, None);
}

fn direct_short_word(mem: &Mem, pc_sw: u32) -> Option<u16> {
    mem.read_present(pc_sw.checked_mul(2)?, 2)
        .map(|word| word as u16)
}

#[test]
fn diagnostic_clock_has_an_explicit_continuation_base() {
    let mut e = crate::Engine::new(Mem::new());
    assert_eq!(e.set_option(6, -1), -1);
    assert_eq!(e.set_option(6, 0x1_0000_0002), 0);
    assert_eq!(e.set_option(5, 1), 0);
    // A failed decode still exposes this attempted instruction's tick;
    // it does not advance the completed-instruction counter.
    assert_eq!(e.step(1), 0);
    assert_eq!(e.s.r[105], V::c(2));
    assert_eq!(e.s.r[106], V::c(1));
    assert_eq!(e.s.icount, 0);
}

#[test]
fn runtime_decode_cache_is_owned_and_invalidated_per_engine() {
    // 0x0000, 0x0001 decodes as the runtime symbolized 21p_undoc16 form.
    let mut mem = Mem::new();
    mem.load(0, &[0, 0, 1, 0]);
    mem.reset();
    let mut e = crate::Engine::new(mem);
    e.enable_runtime_decode(direct_short_word);
    let decoded = crate::decode::decode_at(|pc| direct_short_word(&e.s.mem, pc), 0);
    assert_eq!(decoded.type_name, "21p_undoc16");
    assert_eq!(crate::sym_of(decoded.type_name), Some(S_21P_UNDOC16));
    assert!(crate::sym_of("operand").is_some());
    assert!(crate::sym_of("operand[6:0]").is_some());
    let first = bnd::decode_at(&mut e.s, (), None, 0).unwrap();
    assert_eq!(first.type_name, S_21P_UNDOC16);
    assert_eq!(e.s.decode_cache.len(), 1);
    assert_eq!(
        bnd::decode_at(&mut e.s, (), None, 0).unwrap().type_name,
        first.type_name
    );
    // Guest stores and host pokes are observed without an explicit cache clear.
    e.s.mem.write_byte(0, 0xff);
    e.s.mem.write_byte(1, 0xff);
    assert!(bnd::decode_at(&mut e.s, (), None, 0).is_err());
}

#[test]
fn normal_word_aliases_preserve_access_context_and_rollback() {
    let mut mem = Mem::new();
    mem.load(
        0x80000000,
        &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
    );
    mem.load(0x10000000, &[0xef, 0xbe, 0xad, 0xde]);
    mem.reset();
    let mut s = St::new(mem);
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x10000000), 4, false, true).unwrap(),
        Some(V::c(0x44332211))
    );
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x10000001), 4, false, true).unwrap(),
        Some(V::c(0x88776655))
    );
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x10000000), 4, false, false).unwrap(),
        Some(V::c(0xdeadbeef))
    );
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x10000000), 2, false, false).unwrap(),
        Some(V::c(0xbeef))
    );
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(0x10000001), 4, V::c(0x12345678), true).unwrap());
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x80000004), 4, false, false).unwrap(),
        Some(V::c(0x12345678))
    );
    s.rollback();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x10000001), 4, false, true).unwrap(),
        Some(V::c(0x88776655))
    );
    assert!(s.mem.dirty_bytes().is_empty());
}

#[test]
fn runtime_ffi_selection_and_loaded_execution_aliases() {
    let mut mem = Mem::new();
    mem.load(0x28380000, &[0, 0, 1, 0]);
    mem.load(0x20000000, &[0x11, 0x22]);
    mem.reset();
    assert_eq!(mem.read_sw(0xb80000), Some(0x2211));
    assert_eq!(mem.read_sw(0x1c0000), Some(0));
    assert_eq!(mem.read_sw(1 << 24), None);
    let mut e = crate::Engine::new(mem);
    // The configuration used by the C interface selects the concrete reader.
    assert_eq!(e.set_option(4, 1), 0);
    let first = bnd::decode_at(&mut e.s, (), None, 0x1c0000).unwrap();
    assert_eq!(first.type_name, S_21P_UNDOC16);
    assert_eq!(e.poke(0x28380000, &[0xff, 0xff], 2), 1);
    assert!(bnd::decode_at(&mut e.s, (), None, 0x1c0000).is_err());
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
    let v = bnd::_dm_read(&s, VI::I(0x1000), 4, false, false).unwrap();
    assert_eq!(v, Some(V::c(0x0403_0201)));
    // A signed byte read sign-extends, then masks to 32 bits.
    s.mem.load(0x2800_2000, &[0x80]);
    s.mem.reset();
    let v = bnd::_dm_read(&s, VI::I(0x2000), 1, true, false).unwrap();
    assert_eq!(v, Some(V::c(0xFFFF_FF80)));
    // Unwritten internal RAM reads as 0 under the explicit memory model.
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x5000), 4, false, false).unwrap(),
        Some(V::c(0))
    );
    // A write to an unbacked low address lands at the alias.
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(0x6000), 2, V::c(0xBEEF), false).unwrap());
    s.commit();
    assert_eq!(s.mem.byte(0x2800_6000), 0xEF);
    assert!(!s.mem.present(0x6000));
    assert_eq!(
        s.mem.dirty_bytes(),
        vec![(0x2800_6000, 0xEF), (0x2800_6001, 0xBE)]
    );
    // An unknown value is not stored.
    assert!(!bnd::_dm_write(&mut s, VI::I(0x6000), 4, V::UNK, false).unwrap());
}

#[test]
fn memory_write_rolls_back() {
    let mut s = state();
    s.begin();
    bnd::_dm_write(&mut s, VI::I(0x1000), 1, V::c(0x55), false).unwrap();
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
        bnd::_dm_read(&s, VI::I(0x3100_0000), 4, false, false),
        Err(TRAP_UNMODELED_MMR)
    );
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(0x3100_0000), 4, V::c(5), false).unwrap());
    s.commit();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(0x3100_0000), 4, false, false),
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
    bnd::_dm_write(&mut s, VI::I(0x7000), 4, V::c(0xAABBCCDD), false).unwrap();
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
        bnd::_dm_read_b(&s, VI::I(0x1000), 4, false, false).unwrap(),
        bnd::_dm_read_full(&s, VI::I(0x1000), 4, false, false).unwrap()
    );
    // A write lands at the alias (once its bytes are overlay bytes, the
    // fast path takes it too).
    assert!(bnd::_dm_write_nolog_b(&mut s, VI::I(0x1000), 4, V::c(0x0a0b_0c0d), false).unwrap());
    assert_eq!(s.mem.read_le(0x2800_1000, 4), 0x0a0b_0c0d);
    assert_eq!(s.mem.fast_write(0x1000, 4, 0x1111_2222), Some(0x0a0b_0c0d));
    assert_eq!(s.mem.read_le(0x2800_1000, 4), 0x1111_2222);
    // No fast path on an MMR page.
    s.named_mmrs = vec![0x2800_1000];
    s.set_mmr_windows();
    assert_eq!(s.mem.fast_read(0x2800_1000, 4), None);
}
