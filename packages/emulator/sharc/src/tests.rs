//! Runtime tests (no firmware, no generated code).

use crate::CfgRefresh;
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
fn astatx_define_matches_branch_reference_for_all_value_shapes() {
    fn reference(old: V, mask: Int, bits: Int) -> V {
        let mask = mask as u32;
        let bits = bits as u32 & mask;
        if old.is_c() {
            return V::c(((old.b & !mask) | bits) as Int);
        }
        if old.is_partial() {
            let new_mask = old.m | mask;
            let new_bits = (old.b & !mask) | bits;
            return V {
                b: new_bits & new_mask,
                m: new_mask,
            };
        }
        if mask == 0 {
            return old;
        }
        V { b: bits, m: mask }
    }

    let s = state();
    let values = [
        V::UNK,
        V { b: 0xFFFF_FFFF, m: 0 },
        V::partial(0x0000_0001, 0x0000_0001),
        V::partial(0x8000_0042, 0x8000_0002),
        V { b: 0xFFFF_FFFF, m: 0x0000_0042 },
        V::c(0),
        V::c(0x8000_0042),
        V { b: 0xFFFF_FFFF, m: u32::MAX },
    ];
    let wide = (1i128 << 96) | (1i128 << 47) | 0x8000_0042;
    let masks = [0, 1, -1, 0x0000_0042, 0xFFFF_0000, wide, -wide];
    let bits = [0, -1, 0x8000_0042, wide, -wide];
    for old in values {
        for mask in masks {
            for bits in bits {
                assert_eq!(
                    bnd::_astatx_define(&s, old, mask, bits),
                    reference(old, mask, bits),
                    "old={old:?} mask={mask:#x} bits={bits:#x}"
                );
            }
        }
    }
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
    rf.allow_unknown = true;
    rf_set(&mut rf, 4, V::UNK).unwrap();
    assert_eq!(rf.r[4], V::UNK);
    let partial = V { b: 5, m: 7 };
    rf_set(&mut rf, 4, partial).unwrap();
    assert_eq!(rf.r[4], partial);
    assert_eq!(rf_set(&mut rf, NUREG as Int, V::UNK), Err(TRAP_INDEX));
}

#[test]
fn block_unknown_load_matches_interpreter_value() {
    let mut s = St::new(Mem::new());
    s.cfg.explicit_memory_model = false;
    s.cfg.refresh();
    let mut rf = Rf::default();
    rf.r[4] = V::c(123);
    assert_eq!(
        bnd::_load_normal_ureg_rf(&mut s, &mut rf, S_DM, VI::I(0x2800_1000), 4),
        Err(TRAP_BLOCK_UNKNOWN)
    );
    assert_eq!(rf.r[4], V::c(123));
    rf.allow_unknown = true;
    assert_eq!(
        bnd::_load_normal_ureg_rf(&mut s, &mut rf, S_DM, VI::I(0x2800_1000), 4),
        Ok(None)
    );
    assert_eq!(rf.r[4], V::UNK);
    s.r[4] = V::c(123);
    assert_eq!(
        bnd::_load_normal_ureg(&mut s, S_DM, VI::I(0x2800_1000), 4),
        Ok(None)
    );
    assert_eq!(rf.r[4], V::UNK);
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

#[test]
fn peripheral_model_acknowledges_and_ends_sec_sources() {
    let mut s = state();
    s.set_mmr_windows();
    s.cfg.peripheral_model = true;
    s.cfg.refresh();
    s.mmr_put(periph::SEC_GCTL, V::c(1));
    s.mmr_put(periph::SEC_CCTL, V::c(1));
    s.mmr_put(periph::SEC_SCTL + 8 * 70, V::c(5));
    let status = periph::SEC_SCTL + 8 * 70 + 4;
    s.begin();
    periph::sec_raise(&mut s, 70).unwrap();
    s.commit();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(periph::SECI_ID as Int), 4, false, false),
        Ok(Some(V::c(70)))
    );
    // A rolled-back instruction leaves the source issued and unacknowledged.
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SECI_ID as Int), 4, V::c(0), false).unwrap());
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SEC_END as Int), 4, V::c(70), false).unwrap());
    s.rollback();
    assert_eq!(s.mmr_get(status), Some(V::c(0x100)));
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SECI_ID as Int), 4, V::c(0), false).unwrap());
    assert_eq!(s.mmr_get(status), Some(V::c(0x200)));
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SEC_END as Int), 4, V::c(70), false).unwrap());
    s.commit();
    assert_eq!(s.mmr_get(status), Some(V::c(0)));
    // With the model off the same write is a plain register store.
    s.cfg.peripheral_model = false;
    s.cfg.refresh();
    s.begin();
    s.named_mmrs = vec![periph::SEC_END];
    s.set_mmr_windows();
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SEC_END as Int), 4, V::c(70), false).unwrap());
    s.commit();
    assert_eq!(s.mmr_get(periph::SEC_END), Some(V::c(70)));
}

/// Descriptor rings for SPORT4A (DMA10, TX) and SPORT4B (DMA11, RX) at
/// 0x2810_0000, 4 words each, as the DN2 firmware sets them up.
fn sport_engine() -> crate::Engine {
    const RAM: u32 = 0x2810_0000;
    let words = |v: &[u32]| -> Vec<u8> { v.iter().flat_map(|w| w.to_le_bytes()).collect() };
    let mut image = vec![0u8; 0x400];
    let tx = words(&[RAM, RAM + 0x100, 0x144225, 4, 4]);
    let rx = words(&[RAM + 0x40, RAM + 0x200, 0x144227, 4, 4]);
    image[..tx.len()].copy_from_slice(&tx);
    image[0x40..0x40 + rx.len()].copy_from_slice(&rx);
    image[0x100..0x110].copy_from_slice(&words(&[0xdead_beef, 1, 0x8000_0000, 0x1234_5678]));
    let mut mem = Mem::new();
    mem.load(RAM, &image);
    mem.reset();
    let mut e = crate::Engine::new(mem);
    e.s.cfg.peripheral_model = true;
    e.s.cfg.refresh();
    e.s.mmr_put(periph::SEC_GCTL, V::c(1));
    e.s.mmr_put(periph::SEC_CCTL, V::c(1));
    e.s.mmr_put(periph::SEC_SCTL + 8 * 191, V::c(5));
    e.s.mmr_put(periph::DAI1_GBL_INT_EN, V::c(0x10003));
    e.s.mmr_put(periph::SPORT4A_DMA, V::c(RAM as Int));
    e.s.mmr_put(periph::SPORT4A_DMA + 8, V::c(0x44225));
    e.s.mmr_put(periph::SPORT4B_DMA, V::c((RAM + 0x40) as Int));
    e.s.mmr_put(periph::SPORT4B_DMA + 8, V::c(0x44227));
    e
}

#[test]
fn sport_block_returns_the_tx_unit_and_raises_the_group_source() {
    let mut e = sport_engine();
    let status = periph::SEC_SCTL + 8 * 191 + 4;
    let stat = |e: &crate::Engine, base: u32| e.s.mmr_get(base + 0x30).map(|v| v.b & 0x701);
    // Not running: nothing happens.
    assert_eq!(e.sport_block(None), Ok(None));
    assert_eq!(e.s.mmr_get(status), None);
    // The genuine DN2 start: DAI1 group enable, SPORT CTL.SPEN left clear.
    e.s.mmr_put(periph::DAI1_GBL_SP_EN, V::c(0x5e));
    e.s.mmr_put(periph::SPORT4A_CTL, V::c(0x111f2));
    e.s.mmr_put(periph::SPORT4B_CTL, V::c(0x111f2));
    // A wrong-sized input is rejected and leaves no trace.
    assert!(e.sport_block(Some(&[0; 4])).is_err());
    assert_eq!(e.s.mmr_get(status), None);
    assert_eq!(stat(&e, periph::SPORT4A_DMA), None);
    let input: Vec<u8> = (0..16).collect();
    let out = e.sport_block(Some(&input)).unwrap().unwrap();
    assert_eq!(out[..8], [0xef, 0xbe, 0xad, 0xde, 1, 0, 0, 0]);
    assert_eq!(out.len(), 16);
    assert_eq!(stat(&e, periph::SPORT4A_DMA), Some(0x201));
    assert_eq!(stat(&e, periph::SPORT4B_DMA), Some(0x201));
    // Only the group source is raised; the channel sources stay quiet.
    assert_eq!(e.s.mmr_get(status), Some(V::c(0x100)));
    assert_eq!(e.s.mmr_get(periph::SEC_SCTL + 8 * 53 + 4), None);
    assert_eq!(e.s.mmr_get(periph::SEC_CSID), Some(V::c(191)));
    assert_eq!(e.s.mem.read_le(0x2810_0200, 4), 0x0302_0100);
}

/// `JUMP 0` at PC 0 (Type8a, non-delayed, relative 0): a one-instruction
/// loop that is a fixed point apart from the clocks.
#[cfg(sharc_gen)]
fn spin_engine(skip: bool, period: i64) -> crate::Engine {
    let mut mem = Mem::new();
    mem.load(0, &[0x3e, 0x07, 0, 0, 0, 0]);
    mem.reset();
    let mut e = crate::Engine::new(mem);
    e.enable_runtime_decode(direct_short_word);
    // Block code is only used under the memory model it was generated for.
    let gen_model = i64::from(crate::gen_explicit_memory_model());
    for (k, v) in [(5, 1), (6, 12345), (10, gen_model), (21, 1)] {
        assert_eq!(e.set_option(k, v), 0);
    }
    // The stand-in block code is not the generated image's code.
    e.code_known = true;
    e.code_ok = true;
    e.code_checked = e.s.mem.code_gen;
    e.s.r[116] = V::c(0x20); // MODE2.TIMEN
    e.s.r[114] = V::c(0); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    e.s.r[110] = V::c(period as Int);
    e.s.r[111] = V::c(period as Int);
    if skip {
        assert_eq!(e.set_option(23, 0), 0);
        assert_eq!(e.set_option(24, 0), 0);
        assert_eq!(e.set_option(25, 0), 0);
    }
    e
}

#[cfg(sharc_gen)]
#[test]
fn idle_skip_matches_plain_stepping_across_timer_events() {
    for period in [0, 1, 2, 7, 100, 1000] {
        let mut plain = spin_engine(false, period);
        let mut fast = spin_engine(true, period);
        for n in [1, 2, 3, 50, 99, 100, 101, 977, 5000, 12345, 1] {
            assert_eq!(plain.step(n), n, "{:?}", plain.halt);
            assert_eq!(fast.step(n), n);
            assert_eq!(plain.s.icount, fast.s.icount);
            assert_eq!(plain.s.pc_sw, fast.s.pc_sw);
            assert_eq!(
                plain.s.steps, fast.s.steps,
                "steps period {period} n {n} {:?}",
                fast.idle_stats
            );
            for code in 0..crate::rt::NUREG {
                assert_eq!(
                    plain.s.r[code], fast.s.r[code],
                    "r{code} period {period} n {n}"
                );
            }
        }
        if period >= 2 {
            assert!(
                fast.idle_stats.iterations > 0,
                "period {period} {:?}",
                fast.idle_stats
            );
        }
        assert_eq!(plain.idle_stats.iterations, 0);
    }
}

/// A stand-in for block code over `spin_engine`'s `JUMP 0`: it completes
/// instructions (icount and steps, the PC stays) until its limit, as a
/// generated block does.
#[cfg(sharc_gen)]
fn fake_spin_block(s: &mut St) -> u32 {
    loop {
        if s.icount + 1 > s.limit {
            return crate::EXIT_BUDGET;
        }
        s.icount += 1;
        s.steps += 1;
    }
}

#[cfg(sharc_gen)]
fn same_state(plain: &crate::Engine, fast: &crate::Engine, why: &str) {
    assert_eq!(plain.s.icount, fast.s.icount, "{why}");
    assert_eq!(plain.s.pc_sw, fast.s.pc_sw, "{why}");
    assert_eq!(plain.s.steps, fast.s.steps, "{why}");
    for code in 0..crate::rt::NUREG {
        assert_eq!(plain.s.r[code], fast.s.r[code], "r{code} {why}");
    }
}

/// A model-safe block runs with the core timer and the instruction clock on
/// and leaves exactly what stepping leaves (TCOUNT, the IRPTL latch at
/// expiry, EMUCLK), across odd step sizes and timer periods; a block that is
/// not model-safe never runs.
#[cfg(sharc_gen)]
#[test]
fn model_safe_block_matches_stepping_across_timer_events() {
    for period in [0, 1, 2, 3, 7, 100, 1000] {
        let mut plain = spin_engine(false, period);
        plain.use_blocks = false;
        let mut fast = spin_engine(false, period);
        fast.dispatch = crate::Dispatch::new(&[(0, fake_spin_block)], &[0]);
        let mut unsafe_block = spin_engine(false, period);
        unsafe_block.dispatch = crate::Dispatch::new(&[(0, fake_spin_block)], &[]);
        for n in [1, 2, 3, 50, 99, 100, 101, 977, 5000, 12345, 1] {
            assert_eq!(plain.step(n), n, "{:?}", plain.halt);
            assert_eq!(fast.step(n), n, "{:?}", fast.halt);
            assert_eq!(unsafe_block.step(n), n);
            same_state(&plain, &fast, &format!("period {period} n {n}"));
            same_state(
                &plain,
                &unsafe_block,
                &format!("unsafe period {period} n {n}"),
            );
        }
        if period >= 3 {
            assert!(fast.stats.block_instructions > 0, "period {period}");
            assert!(fast.model_stats.gated > 0);
        }
        assert_eq!(unsafe_block.stats.block_entries, 0);
        assert!(unsafe_block.model_stats.unsafe_block > 0);
        // The timer expired inside stepping, never inside a block.
        assert_eq!(plain.s.r[122], fast.s.r[122], "IRPTL period {period}");
    }
}

/// An interrupt source that is latched and enabled keeps blocks out even
/// where the interpreter defers it (an active DO loop): the deferral could
/// end inside the block.
#[cfg(sharc_gen)]
#[test]
fn latched_interrupt_blocks_model_safe_blocks_while_deferred() {
    let mut e = spin_engine(false, 1000);
    e.dispatch = crate::Dispatch::new(&[(0, fake_spin_block)], &[0]);
    assert_eq!(e.set_option(9, 1), 0);
    assert_eq!(e.latent_interrupt(), Some(false));
    e.s.r[114] = V::c(0x1000); // MODE1.IRPTEN
    e.s.r[122] = V::c(0x1000_0000); // IRPTL: SFT0
    e.s.r[123] = V::c(0x1000_0000); // IMASK
    assert_eq!(e.latent_interrupt(), Some(true));
    e.s.r[124] = V::c(0x1000_0000); // IMASKP: that level is being serviced
    e.s.r[114] = V::c(0x1800); // nesting on: only higher priority is eligible
    assert_eq!(e.latent_interrupt(), Some(false));
    e.s.r[124] = V::c(0x2000_0000);
    assert_eq!(e.latent_interrupt(), Some(true));
    e.s.r[122] = V::UNK;
    assert_eq!(e.latent_interrupt(), None);
}

#[test]
fn peripheral_stores_that_act_leave_block_code() {
    use crate::rt::periph::write_acts;
    for a in [
        periph::SECI_ID,
        periph::SEC_CSID,
        periph::SEC_END,
        periph::SEC_RAISE,
        periph::SEC_SCTL + 4,
        periph::SEC_SCTL + 8 * 70 + 4,
        periph::SPORT4A_DMA + 8,
        periph::SPORT4B_DMA + 0x30,
    ] {
        assert!(write_acts(a), "{a:#x}");
    }
    for a in [
        periph::SEC_SCTL,
        periph::SEC_SCTL + 8 * 70,
        periph::SEC_CCTL,
        0x30000,
    ] {
        assert!(!write_acts(a), "{a:#x}");
    }
    let mut s = state();
    s.cfg.peripheral_model = true;
    s.cfg.refresh();
    s.named_mmrs = vec![periph::SEC_END, 0x3100_0000];
    s.set_mmr_windows();
    s.in_block = true;
    s.begin();
    assert_eq!(
        bnd::_dm_write(&mut s, VI::I(periph::SEC_END as Int), 4, V::c(70), false),
        Err(TRAP_BLOCK_MODEL)
    );
    s.rollback();
    // An ordinary register store goes through; so does the interpreter's.
    assert_eq!(
        bnd::_dm_write(&mut s, VI::I(0x3100_0000), 4, V::c(1), false),
        Ok(true)
    );
    s.in_block = false;
    s.begin();
    assert!(bnd::_dm_write(&mut s, VI::I(periph::SEC_RAISE as Int), 4, V::c(70), false).is_ok());
}

/// The bracketed normal_word_to_byte maps exactly as the plain range table
/// (Rev. D Tables 2--6) it shortcuts: around every bound and on a sweep.
#[test]
fn normal_word_to_byte_bracket_matches_the_table() {
    const T: [(i128, i128, i128); 11] = [
        (0x90000, 0x9c000, 0x28240000),
        (0xb0000, 0xbc000, 0x282c0000),
        (0xc0000, 0xc8000, 0x28300000),
        (0xe0000, 0xe8000, 0x28380000),
        (0x4000000, 0x8000000, 0x60000000),
        (0x8000000, 0x8046000, 0x20000000),
        (0xa090000, 0xa09c000, 0x28240000),
        (0xa0b0000, 0xa0bc000, 0x282c0000),
        (0xa0c0000, 0xa0c8000, 0x28300000),
        (0xa0e0000, 0xa0e8000, 0x28380000),
        (0x10000000, 0x18000000, 0x80000000),
    ];
    let table = |a: i128| {
        T.iter()
            .find(|(lo, hi, _)| (*lo..*hi).contains(&a))
            .map(|(lo, _, base)| base + (a - lo) * 4)
    };
    let mut points: Vec<i128> = vec![-1, 0, 0xe8000, 0x400_0000, 0x1800_0000, 1 << 32];
    for (lo, hi, _) in T {
        for d in -2..=2 {
            points.extend([lo + d, hi + d]);
        }
    }
    points.extend((0..1i128 << 32).step_by(0x1001));
    for a in points {
        assert_eq!(
            crate::addressing::normal_word_to_byte(a),
            table(a),
            "{a:#x}"
        );
    }
}

/// The bank_codes tables are the register groups MODE1's bank bits select
/// (R0-R7/S0-S7, R8-R15/S8-S15, and four of each I/M/L/B group).
#[test]
fn bank_codes_are_the_selected_register_groups() {
    use crate::rt::bank_codes;
    assert_eq!(
        bank_codes(10),
        &[0, 1, 2, 3, 4, 5, 6, 7, 80, 81, 82, 83, 84, 85, 86, 87]
    );
    assert_eq!(
        bank_codes(7),
        &[8, 9, 10, 11, 12, 13, 14, 15, 88, 89, 90, 91, 92, 93, 94, 95]
    );
    assert_eq!(
        bank_codes(4),
        &[
            16, 17, 18, 19, 32, 33, 34, 35, 48, 49, 50, 51, 64, 65, 66, 67
        ]
    );
    assert_eq!(
        bank_codes(3),
        &[
            20, 21, 22, 23, 36, 37, 38, 39, 52, 53, 54, 55, 68, 69, 70, 71
        ]
    );
    assert_eq!(
        bank_codes(6),
        &[
            24, 25, 26, 27, 40, 41, 42, 43, 56, 57, 58, 59, 72, 73, 74, 75
        ]
    );
    assert_eq!(
        bank_codes(5),
        &[
            28, 29, 30, 31, 44, 45, 46, 47, 60, 61, 62, 63, 76, 77, 78, 79
        ]
    );
    assert!(bank_codes(0).is_empty());
}

#[test]
fn chain_stop_set_holds_what_was_added() {
    let mut s = St::new(Mem::new());
    assert!(!s.chain_stops_at(0xb8276c));
    let pcs: Vec<u32> = (0..500).map(|i| 0x1c0000 + i * 7).collect();
    for &pc in &pcs {
        s.chain_stop_add(pc);
        s.chain_stop_add(pc);
    }
    assert!(pcs.iter().all(|&pc| s.chain_stops_at(pc)));
    assert!(!s.chain_stops_at(0x1c0001));
    assert!(!s.chain_stops_at(0xb8276c));
}

/// Two `JUMP 0` self-loops, at PC 0 and PC 3 (three short words each), in
/// the short-word alias. Byte 4 of each instruction is its relative offset.
#[cfg(sharc_gen)]
const SPIN_ALIAS: u32 = 0x2800_0000;

/// An engine running `SPIN_ALIAS` code through the runtime decoder, with the
/// reference fast tier at PC 0 when `fast`.
#[cfg(sharc_gen)]
fn patch_engine(fast: bool, models: bool) -> crate::Engine {
    let spin = [0x3e, 0x07, 0, 0, 0, 0];
    let mut mem = Mem::new();
    mem.load(SPIN_ALIAS, &spin);
    mem.load(SPIN_ALIAS + 6, &spin);
    mem.reset();
    let mut e = crate::Engine::new(mem);
    assert_eq!(e.set_option(4, 1), 0);
    if models {
        let gen_model = i64::from(crate::gen_explicit_memory_model());
        for (k, v) in [(5, 1), (6, 12345), (10, gen_model), (21, 1)] {
            assert_eq!(e.set_option(k, v), 0);
        }
        e.s.r[116] = V::c(0x20); // MODE2.TIMEN
        e.s.r[110] = V::c(100_000);
        e.s.r[111] = V::c(100_000);
    }
    e.s.r[114] = V::c(0); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    if fast {
        e.set_fast(Some(Box::new(crate::fast::FastEngine::new(
            Box::new(crate::fast::interp::InterpBackend),
            &[0],
        ))));
    }
    e
}

/// A fast region is made from the code in memory, so code that changes under
/// it (a host store, a store through the other alias, a restore) must not run
/// from the old region; the result is what the interpreter alone gives.
#[cfg(sharc_gen)]
#[test]
fn fast_region_follows_code_changes() {
    for models in [false, true] {
        let mut plain = patch_engine(false, models);
        let mut fast = patch_engine(true, models);
        // The loop at PC 0 runs from a built region.
        for e in [&mut plain, &mut fast] {
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
        }
        let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
        assert_eq!((st.built, st.runs), (1, 1), "models {models}");
        same_state(&plain, &fast, "before the patch");
        // JUMP 0 -> JUMP 3 (host store): the loop at PC 3 runs, not the old region.
        for e in [&mut plain, &mut fast] {
            e.s.mem.write_byte(SPIN_ALIAS + 4, 3);
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
        }
        assert_eq!(plain.s.pc_sw, 3);
        same_state(&plain, &fast, "after the host store");
        // JUMP 3 -> JUMP 0 again, through the DM write fast path (a byte
        // store by the program itself); the loop at PC 0 is back.
        for e in [&mut plain, &mut fast] {
            e.s.pc_sw = 0;
            assert_eq!(e.s.mem.fast_write(SPIN_ALIAS + 4, 1, 0), Some(3));
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
        }
        assert_eq!(plain.s.pc_sw, 0);
        same_state(&plain, &fast, "after the fast store");
        // A change in an instruction the region never reads leaves it valid.
        let calls = fast.fast.as_ref().unwrap().fast_stats().unwrap().runs;
        for e in [&mut plain, &mut fast] {
            e.s.mem.write_byte(SPIN_ALIAS + 6 + 4, 1);
            assert_eq!(e.step(100), 100, "{:?}", e.halt);
        }
        same_state(&plain, &fast, "after an unrelated store");
        assert!(fast.fast.as_ref().unwrap().fast_stats().unwrap().runs > calls);
    }
}

/// With the loaded code not the code the generated blocks came from, blocks
/// stay off but the fast tier (which checks its own code) keeps running,
/// under the models too (coupled runs of patched DSP code).
#[cfg(sharc_gen)]
#[test]
fn fast_tier_runs_where_blocks_are_barred_by_the_code_check() {
    let mut e = patch_engine(true, true);
    e.dispatch = crate::Dispatch::new(&[(0, fake_spin_block)], &[0]);
    // The stand-in block code is not what the memory holds.
    e.code_known = true;
    e.code_ok = false;
    e.code_checked = e.s.mem.code_gen;
    assert_eq!(e.step(1000), 1000, "{:?}", e.halt);
    let st = e.fast.as_ref().unwrap().fast_stats().unwrap();
    assert!(st.insns > 0, "the fast tier did not run: {st:?}");
    // Every block instruction is the fast tier's (the stand-in never ran).
    assert_eq!(e.stats.block_instructions, st.insns);
    assert!(e.model_stats.code_mismatch > 0);
    // `SHARC_AOT=0`: blocks off, fast tier on.
    let mut e = patch_engine(true, true);
    e.dispatch = crate::Dispatch::new(&[(0, fake_spin_block)], &[0]);
    e.use_blocks = false;
    assert_eq!(e.step(1000), 1000, "{:?}", e.halt);
    let st = e.fast.as_ref().unwrap().fast_stats().unwrap();
    assert!(st.insns > 0);
    assert_eq!(e.stats.block_instructions, st.insns);
}

/// `M4 = R2; DM(M4, I1) = R12; R0 = DM(M4, I1); JUMP -8`, in the short-word
/// alias (each instruction word little endian, in the order of the raw
/// encoding). M4 is written by the loop, so the address has no static range:
/// the fast tier makes a guarded dynamic window anchored on I1.
#[cfg(sharc_gen)]
fn dyn_window_engine(fast: bool, i1: u32, r2: u32, dirty: &[(u32, u32)]) -> crate::Engine {
    let mut mem = Mem::new();
    let code: [u16; 17] = [
        0x703f, 0x123f, // M4 = R2
        0x433e, 0x8600, 0x0000, // DM(M4, I1) = R12
        0x433e, 0x0000, 0x0000, // R0 = DM(M4, I1)
        0x073e, 0x04ff, 0xfff8, // JUMP -8
        0x0000, 0x0000, 0x0000, // the two instructions the jump skips (NOP)
        0x0000, 0x0000, 0x0000,
    ];
    let mut at = SPIN_ALIAS;
    for w in code {
        mem.load(at, &w.to_le_bytes());
        at += 2;
    }
    mem.reset();
    for &(lo, len) in dirty {
        for a in lo..lo + len {
            mem.write_byte(a, (a as u8) ^ 0x5a);
        }
    }
    let mut e = crate::Engine::new(mem);
    assert_eq!(e.set_option(4, 1), 0);
    e.s.r[114] = V::c(0); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    e.s.r[17] = V::c(i1.into());
    e.s.r[2] = V::c(r2.into());
    e.s.r[12] = V::c(0xdead_beef);
    if fast {
        e.set_fast(Some(Box::new(crate::fast::FastEngine::new(
            Box::new(crate::fast::interp::InterpBackend),
            &[0],
        ))));
    }
    e
}

/// A transfer through a guarded dynamic window matches the interpreter: an
/// address inside the span, an address that fails the guard (the kernel
/// leaves before the instruction), and a store window that cannot be made
/// (memory not written yet: the kernel still runs, and leaves at the store).
#[cfg(sharc_gen)]
#[test]
fn guarded_dynamic_window_transfers_match_the_interpreter() {
    const BASE: u32 = 0x10_0000;
    let cases: [(u32, &[(u32, u32)], &str); 5] = [
        (4, &[(BASE, 4096)], "inside the span"),
        (
            0x4000,
            &[(BASE, 4096), (BASE + 0x10000, 4096)],
            "guard fails",
        ),
        (0xffff_fff0, &[(BASE - 64, 4096)], "negative modifier"),
        (4, &[], "store window not resolvable"),
        (0, &[(BASE, 4096)], "zero modifier"),
    ];
    for (r2, dirty, why) in cases {
        let mut plain = dyn_window_engine(false, BASE, r2, dirty);
        let mut fast = dyn_window_engine(true, BASE, r2, dirty);
        for _ in 0..4 {
            assert_eq!(plain.step(97), 97, "{why}: {:?}", plain.halt);
            assert_eq!(fast.step(97), 97, "{why}: {:?}", fast.halt);
            same_state(&plain, &fast, why);
        }
        for a in BASE - 128..BASE + 0x10100 {
            assert_eq!(
                plain.s.mem.byte(a),
                fast.s.mem.byte(a),
                "{why}: byte {a:#x}"
            );
        }
        let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
        assert!(st.built >= 1 && st.insns > 0, "{why}: {st:?}");
    }
}

// -- calls and returns in fast regions (stack model) --------------------------

/// Short-word instruction builders for the call and return sequence tests.
/// A parcel is stored little-endian; PC N is the byte `SPIN_ALIAS + 2 N`.
#[cfg(sharc_gen)]
mod callret_code {
    pub const INC: u16 = 0xc500; // Rd = inc(Rs): | d << 4 | s
    pub const ADD01: u16 = 0xc001; // R0 = add(R0, R1)
    pub const ADD02: u16 = 0xc002; // R0 = add(R0, R2)
    pub const NOP16: u16 = 0x0001;
    /// RETURN (JUMP (M14, I12) (DB)) and its twin through I13 + M13.
    pub const RET: [u16; 2] = [0x083f, 0x343f];
    pub const RET13: [u16; 2] = [0x083f, 0x6c3f];

    pub fn inc(d: u16, s: u16) -> u16 {
        INC | d << 4 | s
    }
    /// CJUMP target (delayed, two slots follow).
    pub fn cjump(t: u32) -> [u16; 3] {
        [0x1804, (t >> 16 & 0xff) as u16, t as u16]
    }
    /// JUMP target (not delayed), relative.
    pub fn jump(pc: u32, t: u32) -> [u16; 3] {
        let off = t.wrapping_sub(pc) & 0xff_ffff;
        [0x073e, (off >> 16) as u16, off as u16]
    }
    /// DO (to the instruction at `end`) UNTIL LCE, trip count in R(r).
    pub fn do_loop(pc: u32, r: u16, end: u32) -> [u16; 3] {
        [0x0d00 | r, 0, (end - pc) as u16]
    }
    pub fn put(code: &mut [u8], pc: u32, parcels: &[u16]) {
        for (k, p) in parcels.iter().enumerate() {
            let at = 2 * (pc as usize + k);
            code[at] = *p as u8;
            code[at + 1] = (*p >> 8) as u8;
        }
    }
}

/// The test program: three callers (A: nested calls; B: a call inside a DO
/// loop; C: a callee that runs its own DO loop before it returns) over three
/// functions F1 (calls F2), F2 and F3.
#[cfg(sharc_gen)]
fn callret_program() -> Vec<u8> {
    use callret_code::*;
    let mut c = vec![0u8; 0x400];
    // A at 0: R1++, CALL F1 (slots R0++, NOP), R0 += R1, JUMP A.
    put(&mut c, 0, &[inc(1, 1)]);
    put(&mut c, 1, &cjump(0x20));
    put(&mut c, 4, &[inc(0, 0), NOP16, ADD01]);
    put(&mut c, 7, &jump(7, 0));
    // F1 at 0x20: R3++, CALL F2 (slots R2++, NOP), R0 += R2, RETURN (slots R0++, NOP).
    put(&mut c, 0x20, &[inc(3, 3)]);
    put(&mut c, 0x21, &cjump(0x40));
    put(&mut c, 0x24, &[inc(2, 2), NOP16, ADD02]);
    put(&mut c, 0x27, &RET);
    put(&mut c, 0x29, &[inc(0, 0), NOP16]);
    // F2 at 0x40: R1++, RETURN through I13 + M13 (slots R0++, NOP).
    put(&mut c, 0x40, &[inc(1, 1)]);
    put(&mut c, 0x41, &RET13);
    put(&mut c, 0x43, &[inc(0, 0), NOP16]);
    // B at 0x60: DO (R15 times) { R1++; CALL F1 (slots R0++, NOP); R0 += R1 }; JUMP B.
    put(&mut c, 0x60, &do_loop(0x60, 15, 0x69));
    put(&mut c, 0x63, &[inc(1, 1)]);
    put(&mut c, 0x64, &cjump(0x20));
    put(&mut c, 0x67, &[inc(0, 0), NOP16, ADD01]);
    put(&mut c, 0x6a, &jump(0x6a, 0x60));
    // F3 at 0x80: DO (R14 times) { R1++; R0++ }; RETURN (slots R0++, NOP).
    put(&mut c, 0x80, &do_loop(0x80, 14, 0x84));
    put(&mut c, 0x83, &[inc(1, 1), inc(0, 0)]);
    put(&mut c, 0x85, &RET);
    put(&mut c, 0x87, &[inc(0, 0), NOP16]);
    // C at 0xa0: R1++, CALL F3 (slots R0++, NOP), R0 += R1, JUMP C.
    put(&mut c, 0xa0, &[inc(1, 1)]);
    put(&mut c, 0xa1, &cjump(0x80));
    put(&mut c, 0xa4, &[inc(0, 0), NOP16, ADD01]);
    put(&mut c, 0xa7, &jump(0xa7, 0xa0));
    c
}

#[cfg(sharc_gen)]
fn callret_engine(fast: bool, start: u32, ret: u32, entries: &[u32]) -> crate::Engine {
    let mut mem = Mem::new();
    mem.load(SPIN_ALIAS, &callret_program());
    mem.reset();
    let mut e = crate::Engine::new(mem);
    assert_eq!(e.set_option(4, 1), 0);
    e.s.cfg.stack_model = true;
    e.s.r[114] = V::c(0); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    e.s.pc_sw = start as Int;
    // Data and address registers known (the kernels require it).
    for (c, v) in e.s.r.iter_mut().enumerate().take(48) {
        *v = V::c(if c < 16 { c as Int * 3 + 1 } else { 0 });
    }
    // The return addresses (I12 + M14 for F1 and F3, I13 + M13 for F2).
    e.s.r[28] = V::c(ret as Int);
    e.s.r[46] = V::c(0);
    e.s.r[29] = V::c(0x26);
    e.s.r[45] = V::c(0);
    e.s.r[15] = V::c(3);
    e.s.r[14] = V::c(2);
    if fast {
        let mut fe =
            crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), entries);
        fe.force_cfg(true);
        e.set_fast(Some(Box::new(fe)));
    }
    e
}

#[cfg(sharc_gen)]
fn callret_same(plain: &crate::Engine, fast: &crate::Engine, why: &str) {
    same_state(plain, fast, why);
    assert_eq!(
        crate::canon::export_state(&plain.s, false),
        crate::canon::export_state(&fast.s, false),
        "{why}"
    );
}

/// Calls, nested calls and delayed returns (alone, with a loop active at
/// the call, and with a loop of the callee before the return) leave exactly
/// the state the interpreter leaves, at every step size, and the regions do
/// not end at the call or the return.
#[cfg(sharc_gen)]
#[test]
fn fast_calls_and_returns_match_the_interpreter() {
    // (name, start, return address of F1/F3, entries)
    let cases: [(&str, u32, u32, &[u32]); 5] = [
        ("nested", 0, 6, &[0, 0x20, 0x40]),
        ("caller only", 0, 6, &[0]),
        ("call in a loop", 0x60, 0x69, &[0x60, 0x63, 0x20, 0x40]),
        ("loop active at the return", 0x60, 0x69, &[0x63]),
        ("callee loop", 0xa0, 0xa6, &[0xa0, 0x80]),
    ];
    for (name, start, ret, entries) in cases {
        // The share of instructions the regions must run (loop cases enter
        // and leave through the interpreter more often).
        let min_pct = if name.contains("loop") { 0 } else { 60 };
        for chunk in [1u32, 3, 7, 50, 400] {
            let mut plain = callret_engine(false, start, ret, entries);
            let mut fast = callret_engine(true, start, ret, entries);
            for k in 0..(1200 / chunk).max(2) {
                for e in [&mut plain, &mut fast] {
                    assert_eq!(e.step(chunk), chunk, "{name}: {:?}", e.halt);
                }
                callret_same(&plain, &fast, &format!("{name}, chunk {chunk}, step {k}"));
            }
            if chunk == 400 {
                let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
                assert!(st.runs > 0 && st.insns > 0, "{name}: {st:?}");
                // No region ends at a call or a return: the fast tier runs
                // nearly everything.
                assert!(
                    st.insns * 100 >= fast.s.icount * min_pct,
                    "{name}: {} of {} insns in regions",
                    st.insns,
                    fast.s.icount
                );
            }
        }
    }
}

// -- fast tier: hardware loops and Type 3d against the interpreter -----------------

/// A raw 48-bit instruction as the three short words the code alias holds
/// (each little-endian, high word first).
#[cfg(sharc_gen)]
fn lp_bytes(code: &[u64]) -> Vec<u8> {
    let mut v = Vec::new();
    for raw in code {
        for w in [(raw >> 32) as u16, (raw >> 16) as u16, *raw as u16] {
            v.extend_from_slice(&w.to_le_bytes());
        }
    }
    v
}

#[cfg(sharc_gen)]
fn lp_2a(cond: u64, opcode: u64, rn: u64, rx: u64, ry: u64) -> u64 {
    let compute = (opcode << 12) | (rn << 8) | (rx << 4) | ry;
    0x0100_0000_0000 | (cond << 33) | compute
}

/// `Rn = Rx + Ry`.
#[cfg(sharc_gen)]
fn lp_add(rn: u64, rx: u64, ry: u64) -> u64 {
    lp_2a(0x1f, 0x01, rn, rx, ry)
}

/// `Rn = Rx - Ry`.
#[cfg(sharc_gen)]
fn lp_sub(rn: u64, rx: u64, ry: u64) -> u64 {
    lp_2a(0x1f, 0x02, rn, rx, ry)
}

/// `ureg = value` (Type 17a).
#[cfg(sharc_gen)]
fn lp_imm(ureg: u64, value: u32) -> u64 {
    0x0f00_0000_0000 | (ureg << 32) | value as u64
}

/// `JUMP (cond)` by REL short words from the jump.
#[cfg(sharc_gen)]
fn lp_jump(cond: u64, rel: i32) -> u64 {
    0x0700_0000_0000 | (cond << 33) | (rel as u32 & 0xff_ffff) as u64
}

/// `LCNTR = count, DO (pc + REL) UNTIL LCE`.
#[cfg(sharc_gen)]
fn lp_do(count: u32, rel: i32) -> u64 {
    0x0c00_0000_0000
        | (((count >> 8) as u64 & 0xff) << 32)
        | ((count as u64 & 0xff) << 24)
        | (rel as u32 & 0x7f_ffff) as u64
}

/// `LCNTR = ureg, DO (pc + REL) UNTIL LCE`.
#[cfg(sharc_gen)]
fn lp_do_reg(ureg: u64, rel: i32) -> u64 {
    0x0d00_0000_0000 | (ureg << 32) | (rel as u32 & 0x7f_ffff) as u64
}

/// Type 3a: `ureg <-> DM(Ii, Mm)`, a normal word.
#[cfg(sharc_gen)]
fn lp_3a(cond: u64, store: bool, post: bool, i: u64, m: u64, ureg: u64) -> u64 {
    0x4000_0000_0000
        | ((post as u64) << 44)
        | (i << 41)
        | (m << 38)
        | (cond << 33)
        | ((store as u64) << 31)
        | (ureg << 23)
}

/// Type 16a: `DM(Ii, Mm) = data32` with a post-modify.
#[cfg(sharc_gen)]
fn lp_16a(i: u64, m: u64, data: u32) -> u64 {
    0x9000_0000_0000 | (i << 41) | (m << 38) | data as u64
}

/// `CALL (pc + REL) (DB)`: a delayed call.
#[cfg(sharc_gen)]
fn lp_call_db(rel: i32) -> u64 {
    0x0700_0000_0000 | (1 << 39) | (0x1f << 33) | (1 << 26) | (rel as u32 & 0xff_ffff) as u64
}

/// Type 3d: `ureg <-> DM(Ii, Mm)` of the byte or short access (l, x), in
/// the ACCESS group (w = 0) without the exclusive monitor (ex = 0).
#[cfg(sharc_gen)]
fn lp_3d(cond: u64, store: bool, post: bool, i: u64, m: u64, ureg: u64, l: u64, x: u64) -> u64 {
    0x4000_0030_0000
        | ((post as u64) << 44)
        | (i << 41)
        | (m << 38)
        | (cond << 33)
        | ((store as u64) << 31)
        | (l << 30)
        | (ureg << 23)
        | (x << 16)
}

#[cfg(sharc_gen)]
const LP_DATA: u32 = 0x25_f000;
#[cfg(sharc_gen)]
const LP_DATA_WORDS: u32 = 0x100;

/// An engine over CODE (instruction `k` at short word `3 * k`), with the
/// fast tier's CFG shape armed at ENTRIES (short-word addresses) when FAST.
#[cfg(sharc_gen)]
fn lp_engine(
    code: &[u64],
    entries: &[u32],
    fast: bool,
    setup: &dyn Fn(&mut crate::Engine),
) -> crate::Engine {
    let mut mem = Mem::new();
    mem.load(SPIN_ALIAS, &lp_bytes(code));
    mem.reset();
    let mut e = crate::Engine::new(mem);
    assert_eq!(e.set_option(4, 1), 0);
    e.s.r[114] = V::c(0); // MODE1
    e.s.r[122] = V::c(0); // IRPTL
    e.s.r[123] = V::c(0); // IMASK
    e.s.r[124] = V::c(0); // IMASKP
    // R, I, M, B and L known (linear buffers), the flags and the stack status too.
    for code in (0..80).chain(118..122) {
        e.s.r[code] = V::c(0);
    }
    for k in 0..LP_DATA_WORDS {
        let a = e.s.mem.canonical_of(LP_DATA + 4 * k);
        for b in 0..4u32 {
            let v = (k as u8).wrapping_mul(37).wrapping_add(b as u8 * 11 + 3);
            e.s.mem.write_byte(a + b, v);
        }
    }
    setup(&mut e);
    if fast {
        let mut fe =
            crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), entries);
        fe.force_cfg(true);
        e.set_fast(Some(Box::new(fe)));
    }
    e
}

/// What the interpreter and the fast tier must agree on after every step.
#[cfg(sharc_gen)]
fn lp_same(plain: &crate::Engine, fast: &crate::Engine, why: &str) {
    let (p, f) = (&plain.s, &fast.s);
    assert_eq!(p.icount, f.icount, "icount {why}");
    assert_eq!(p.pc_sw, f.pc_sw, "pc {why}");
    assert_eq!(p.steps, f.steps, "steps {why}");
    for code in 0..crate::rt::NUREG {
        assert_eq!(p.r[code], f.r[code], "r{code} {why}");
    }
    assert_eq!(p.loops.items(), f.loops.items(), "loop stack {why}");
    assert_eq!(p.loop_depth, f.loop_depth, "loop depth {why}");
    assert_eq!(p.loop_slots.items(), f.loop_slots.items(), "loop slots {why}");
    assert_eq!(p.pc_stack.items(), f.pc_stack.items(), "pc stack {why}");
    assert_eq!(p.call_stack.items(), f.call_stack.items(), "call stack {why}");
    assert_eq!(p.pending, f.pending, "pending {why}");
    for k in 0..LP_DATA_WORDS * 4 {
        let a = p.mem.canonical_of(LP_DATA + k);
        assert_eq!(p.mem.byte(a), f.mem.byte(a), "data byte {k:#x} {why}");
    }
}

/// Run CODE on the interpreter and on the fast tier in chunks of each size
/// below, comparing after every chunk. Returns the instructions the fast
/// tier ran (the most over the chunk sizes), asserting it ran kernels.
#[cfg(sharc_gen)]
fn lp_check(
    name: &str,
    code: &[u64],
    entries: &[u32],
    total: u32,
    setup: &dyn Fn(&mut crate::Engine),
) -> u64 {
    let ran = lp_compare(name, code, entries, total, setup);
    assert!(ran > 0, "{name}: the fast tier never ran");
    ran
}

/// `lp_check` without the requirement that the fast tier ran (a case where
/// the lowering must refuse).
#[cfg(sharc_gen)]
fn lp_compare(
    name: &str,
    code: &[u64],
    entries: &[u32],
    total: u32,
    setup: &dyn Fn(&mut crate::Engine),
) -> u64 {
    let mut ran = 0;
    for chunk in [1u32, 2, 3, 5, 7, 11, 64, 1000] {
        let mut plain = lp_engine(code, entries, false, setup);
        let mut fast = lp_engine(code, entries, true, setup);
        let mut done = 0u32;
        while done < total {
            let n = chunk.min(total - done);
            assert_eq!(plain.step(n), n, "{name}: {:?}", plain.halt);
            assert_eq!(fast.step(n), n, "{name}: {:?}", fast.halt);
            done += n;
            lp_same(&plain, &fast, &format!("{name}: chunk {chunk}, after {done}"));
        }
        let st = fast.fast.as_ref().unwrap().fast_stats().unwrap();
        ran = ran.max(st.insns);
    }
    ran
}

#[cfg(sharc_gen)]
fn lp_regs(e: &mut crate::Engine, set: &[(usize, u32)]) {
    for &(r, v) in set {
        e.s.r[r] = V::c(v as Int);
    }
}

/// Short-word address of instruction K of a program of 48-bit instructions.
#[cfg(sharc_gen)]
fn lp_pc(k: u32) -> u32 {
    3 * k
}

/// Short words from instruction FROM to instruction TO.
#[cfg(sharc_gen)]
fn lp_rel(from: i32, to: i32) -> i32 {
    3 * (to - from)
}

/// `Fn = Fx * Fy`.
#[cfg(sharc_gen)]
fn lp_fmul(rn: u64, rx: u64, ry: u64) -> u64 {
    lp_2a(0x1f, 0x130, rn, rx, ry)
}

/// `Fn = Fx + Fy` and `Fn = Fx - Fy`.
#[cfg(sharc_gen)]
fn lp_fadd(rn: u64, rx: u64, ry: u64) -> u64 {
    lp_2a(0x1f, 0x081, rn, rx, ry)
}

#[cfg(sharc_gen)]
fn lp_fsub(rn: u64, rx: u64, ry: u64) -> u64 {
    lp_2a(0x1f, 0x082, rn, rx, ry)
}

/// Denormal operands read as zeros and denormal results are returned as zeros
/// of their sign (`floats.py`, "Denormal flush"): the fast tier must leave
/// the interpreter's state, ASTATX and STKYX included, for float multiplies
/// and adds whose operand or result is a denormal, a product that underflows
/// to zero, a sum that cancels into the denormal range, and the neighbours
/// that stay normal.
#[cfg(sharc_gen)]
#[test]
fn fast_denormal_float_ops_match_the_interpreter() {
    let code = [
        lp_fmul(10, 6, 6),              // 0: F10 = 1.0 * 1.0 (a normal result)
        lp_fmul(2, 0, 1),               // 1: F2 = F0 * F1 (2^-140: denormal product)
        lp_fmul(3, 2, 1),               // 2: F3 = F2 * F1
        lp_fmul(4, 0, 0),               // 3: F4 = F0 * F0 (2^-200: underflow to zero)
        lp_fmul(5, 6, 1),               // 4: F5 = F6 * F1 (a normal result)
        lp_fmul(7, 8, 9),               // 5: F7 = F8 * F9 (a negative denormal product)
        lp_fmul(2, 13, 6),              // 6: F2 = F13 * 1.0 (a denormal operand: zero)
        lp_fmul(14, 13, 15),            // 7: F14 = F13 * 2^97 (a denormal operand: zero)
        lp_fadd(3, 13, 6),              // 8: F3 = F13 + 1.0 (a denormal operand: 1.0)
        lp_fsub(4, 11, 12),             // 9: F4 = 1.5*2^-126 - 2^-126 (a denormal difference: +0)
        lp_fsub(5, 12, 11),             // 10: F5 = 2^-126 - 1.5*2^-126 (a negative one: -0)
        lp_fadd(7, 11, 12),             // 11: F7 = 1.5*2^-126 + 2^-126 (normal)
        lp_2a(0x06, 0x01, 10, 10, 6),   // 12: IF MS R10 = R10 + R6 (reads MN of the last multiply)
        lp_jump(0x1f, lp_rel(13, 0)),   // 13: back to 0
    ];
    let set = |e: &mut crate::Engine| {
        lp_regs(
            e,
            &[
                (0, 0x0d80_0000),  // 2^-100
                (1, 0x2b80_0000),  // 2^-40
                (6, 0x3f80_0000),  // 1.0
                (8, 0x8d80_0000),  // -2^-100
                (9, 0x2b80_0000),  // 2^-40
                (11, 0x00c0_0000), // 1.5 * 2^-126
                (12, 0x0080_0000), // 2^-126
                (13, 0x0000_0123), // a denormal
                (15, 0x7080_0000), // 2^97
            ],
        )
    };
    lp_check("denormal float ops", &code, &[0], 400, &set);
}

/// DO loops nested two deep, started in the region (entry at the top), and
/// entered mid-body (the first instruction of the outer body, inside the
/// inner loop where both are active at entry, at each loop's last
/// instruction).
#[cfg(sharc_gen)]
#[test]
fn fast_nested_loops_match_the_interpreter() {
    let code = [
        lp_imm(0, 0),                 // 0: R0 = 0
        lp_imm(1, 0),                 // 1: R1 = 0
        lp_do(5, lp_rel(2, 7)),       // 2: DO (outer end 7) 5 times
        lp_add(1, 1, 2),              // 3: R1 += R2
        lp_do(3, lp_rel(4, 6)),       // 4: DO (inner end 6) 3 times
        lp_add(0, 0, 1),              // 5: R0 += R1
        lp_add(3, 3, 2),              // 6: R3 += R2   (inner end)
        lp_add(4, 4, 2),              // 7: R4 += R2   (outer end)
        lp_jump(0x1f, 0),             // 8: spin
    ];
    let set = |e: &mut crate::Engine| lp_regs(e, &[(2, 1)]);
    for entries in [0, 3, 5, 6, 7].map(|k| vec![lp_pc(k)]) {
        lp_check(&format!("nested {entries:?}"), &code, &entries, 160, &set);
    }
    // Every instruction an entry: regions of all shapes at once.
    let all: Vec<u32> = (0..9).map(lp_pc).collect();
    lp_check("nested all", &code, &all, 160, &set);
}

/// A loop of one iteration, and loops whose count is a register, started in
/// the region and entered mid-body.
#[cfg(sharc_gen)]
#[test]
fn fast_single_iteration_and_register_count_loops_match() {
    let code = [
        lp_imm(5, 1),                 // 0: R5 = 1
        lp_do(1, lp_rel(1, 3)),       // 1: DO (end 3) once
        lp_add(0, 0, 2),              // 2
        lp_add(1, 1, 2),              // 3: end
        lp_do_reg(5, lp_rel(4, 6)),   // 4: DO (end 6) R5 times
        lp_add(2, 2, 6),              // 5: R2 += R6
        lp_add(3, 3, 2),              // 6: end
        lp_do_reg(7, lp_rel(7, 9)),   // 7: DO (end 9) R7 times
        lp_add(4, 4, 2),              // 8
        lp_add(5, 5, 2),              // 9: end
        lp_jump(0x1f, 0),             // 10: spin
    ];
    for count in [1u32, 2, 4] {
        let set = move |e: &mut crate::Engine| lp_regs(e, &[(2, 1), (6, 3), (7, count)]);
        for entries in [0, 2, 3, 5, 8, 9].map(|k| vec![lp_pc(k)]) {
            lp_check(&format!("single {count} {entries:?}"), &code, &entries, 80, &set);
        }
    }
}

/// A loop whose register count is exact relative to the entry registers
/// (`R5 = R7 - 2`, or `R7 = R7 - 2`), stepping two pointers, with a pointer
/// read after the loop: the region's iteration count comes from the register
/// at entry. Counts of 0 and past the end of the data decline or leave; every
/// run must match the interpreter.
#[cfg(sharc_gen)]
#[test]
fn fast_register_count_loop_with_stepped_pointers_matches() {
    let at = |k: u32| LP_DATA + k;
    for same in [false, true] {
        let cnt = if same { 7 } else { 5 };
        let code = [
            lp_imm(6, 2),                                  // 0: R6 = 2
            lp_sub(cnt, 7, 6),                             // 1: count = R7 - R6
            lp_do_reg(cnt, lp_rel(2, 6)),                  // 2: DO (end 6) count times
            lp_3d(0x1f, false, true, 0, 0, 4, 0, 0),       // 3: R4 = DM(I0, M0) byte, post
            lp_3d(0x1f, true, true, 1, 0, 4, 0, 0),        // 4: DM(I1, M0) = R4 byte, post
            lp_add(3, 3, 4),                               // 5: R3 += R4
            lp_add(9, 9, 2),                               // 6: R9 += R2   (end)
            lp_3d(0x1f, false, true, 0, 0, 8, 0, 0),       // 7: R8 = DM(I0, M0) after the loop
            lp_3d(0x1f, true, true, 1, 0, 8, 0, 0),        // 8: DM(I1, M0) = R8 after the loop
            lp_jump(0x1f, 0),                              // 9: spin
        ];
        let all: Vec<u32> = (0..10).map(lp_pc).collect();
        // The region's trip count is R7 - 2, evaluated at entry.
        let probe = lp_engine(&code, &[], false, &|e| {
            lp_regs(e, &[(2, 1), (7, 9), (16, at(0x10)), (17, at(0x200)), (32, 1)])
        });
        let region = crate::fast::region::build_cfg(&probe.s, lp_pc(0)).expect("region");
        assert_eq!(region.cfg.as_ref().unwrap().reg_n, Some((7, -2)), "same={same}");
        for n in [1u32, 2, 3, 7, 40] {
            let set = move |e: &mut crate::Engine| {
                lp_regs(
                    e,
                    &[(2, 1), (7, n + 2), (16, at(0x10)), (17, at(0x200)), (32, 1)],
                )
            };
            let total = 3 * n + 12;
            for entries in [vec![0], vec![lp_pc(1)], vec![lp_pc(2)], all.clone()] {
                lp_check(&format!("regcount same={same} n={n} {entries:?}"), &code, &entries, total, &set);
            }
        }
        // A count of 1, one whose windows run past the data, and a huge one.
        for r7 in [3u32, 3_000, 0x1000_0002] {
            let set = move |e: &mut crate::Engine| {
                lp_regs(
                    e,
                    &[(2, 1), (7, r7), (16, at(0x10)), (17, at(0x200)), (32, 1)],
                )
            };
            lp_compare(&format!("regcount edge same={same} r7={r7:#x}"), &code, &all, 40, &set);
        }
    }
}

/// The register-count loop's windows must span exactly `R7 - 2` iterations.
/// A window is a host pointer into one 64 KiB page, so a window that is one
/// byte short is harmless until the last access crosses the page end: the
/// exact window then does not fit the page (the fast tier declines and the
/// interpreter runs it), while a short one would run the kernel past the
/// page buffer. A stepped load pointer or a stepped store pointer ends its
/// last access just before, on, or just after the page end; the byte stored
/// last is read back after the loop. A mutant with `iters = n - 1` fails here.
#[cfg(sharc_gen)]
#[test]
fn fast_register_count_loop_windows_end_exactly_at_the_page_boundary() {
    let code = [
        lp_imm(6, 2),                            // 0: R6 = 2
        lp_sub(5, 7, 6),                         // 1: R5 = R7 - R6
        lp_do_reg(5, lp_rel(2, 5)),              // 2: DO (end 5) R5 times
        lp_3d(0x1f, false, true, 0, 0, 4, 0, 0), // 3: R4 = DM(I0, M0) byte, post
        lp_3d(0x1f, true, true, 1, 0, 4, 0, 0),  // 4: DM(I1, M0) = R4 byte, post
        lp_add(3, 3, 4),                         // 5: R3 += R4   (end)
        lp_3d(0x1f, false, true, 2, 0, 8, 0, 0), // 6: R8 = DM(I2, M0) byte (the last store)
        lp_jump(0x1f, 0),                        // 7: spin
    ];
    let at = |k: u32| LP_DATA + k;
    let end = 0x26_0000u32; // the page end after LP_DATA
    let entries: Vec<u32> = (0..8).map(lp_pc).collect();
    let mut ran_total = 0;
    for pointer in [0u32, 1] {
        for n in [3u32, 6, 9] {
            // The last access is at `end - 1 + d`: d = 0 fits the page
            // exactly, d = 1 is the first byte of the next page.
            for d in [-1i64, 0, 1] {
                let start = (end as i64 - n as i64 + d) as u32;
                let set = move |e: &mut crate::Engine| {
                    let (i0, i1, i2) = if pointer == 0 {
                        (start, at(0x200), at(0x300))
                    } else {
                        (at(0x10), start, start + n - 1)
                    };
                    lp_regs(e, &[(2, 1), (7, n + 2), (16, i0), (17, i1), (18, i2), (32, 1)]);
                    // Distinct non-zero bytes either side of the boundary.
                    for k in 0..64u32 {
                        let ca = e.s.mem.canonical_of(end - 32 + k);
                        e.s.mem.write_byte(ca, (k as u8).wrapping_mul(13).wrapping_add(0x55));
                    }
                };
                ran_total += lp_compare(
                    &format!("regcount page end ptr={pointer} n={n} d={d}"),
                    &code,
                    &entries,
                    3 * n + 8,
                    &set,
                );
            }
        }
    }
    assert!(ran_total > 0, "the fast tier never ran");
}

/// The return-address push in the delay slot of a call (`DM(I7, M7) = imm`,
/// Type 16a) where I7 has no static range (it steps in a backward-branch
/// cycle before): the push is a guarded dynamic-window store. Pushes inside
/// the window, at its edge and outside plain memory must all match.
#[cfg(sharc_gen)]
#[test]
fn fast_call_slot_push_through_an_unranged_stack_pointer_matches() {
    let at = |k: u32| LP_DATA + k;
    // The stack model, with loaded calls followed (a CALL ends a kernel).
    let stack_calls = |e: &mut crate::Engine| {
        for (opt, v) in [(13, 1), (14, 16), (20, 1)] {
            assert_eq!(e.set_option(opt, v), 0);
        }
    };
    let code = [
        lp_imm(4, 3),                          // 0: R4 = 3
        lp_3a(0x1f, true, true, 7, 7, 0),      // 1: DM(I7, M7) = R0 (post)   <- cycle head
        lp_sub(4, 4, 2),                       // 2: R4 -= R2 (AZ)
        lp_jump(0x10, lp_rel(3, 1)),           // 3: IF NE JUMP 1
        lp_add(0, 0, 2),                       // 4: R0 += R2
        lp_call_db(lp_rel(5, 9)),              // 5: CALL 9 (DB)
        lp_16a(7, 7, 0x1234_5678),             // 6: slot 1: DM(I7, M7) = imm (post)
        lp_add(1, 1, 2),                       // 7: slot 2: R1 += R2
        lp_jump(0x1f, 0),                      // 8: spin (the return point)
        lp_add(3, 3, 2),                       // 9: the callee
        lp_jump(0x1f, 0),                      // 10: spin
    ];
    let all: Vec<u32> = (0..11).map(lp_pc).collect();
    // The push is lowered in the kernel (not the end of the region).
    let probe = lp_engine(&code, &[], false, &|e| {
        stack_calls(e);
        lp_regs(e, &[(2, 1), (23, at(0x300)), (39, 0xffff_ffff)])
    });
    let region = crate::fast::region::build_cfg(&probe.s, lp_pc(0)).expect("region");
    let stops = &region.cfg.as_ref().unwrap().stops;
    assert!(
        !stops.iter().any(|(pc, _)| *pc == lp_pc(5) || *pc == lp_pc(6)),
        "the call with the 16a push became a stop: {stops:?}"
    );
    for (sp, m7) in [
        (at(0x300), 0xffff_ffffu32), // downward in the window
        (at(0x100), 1),              // upward
        (at(0x3f0), 1),              // past the end of the data window
        (at(0x8), 0xffff_ffff),      // below the low edge of the data
        (0x1000, 0xffff_ffff),       // not in the data window at all
    ] {
        let set = move |e: &mut crate::Engine| {
            stack_calls(e);
            lp_regs(e, &[(2, 1), (23, sp), (39, m7)])
        };
        lp_compare(&format!("push sp={sp:#x} m7={m7:#x}"), &code, &all, 40, &set);
    }
}

/// A loop active at entry whose range does not hold the entry: the region
/// starts in an out-of-line block that jumps back to the loop's last
/// instruction (as the compiler's cold paths do). The loop pops in the
/// kernel, which leaves there.
#[cfg(sharc_gen)]
#[test]
fn fast_outer_active_loop_end_reached_from_outside_matches() {
    for count in [1u32, 2, 4] {
        let code = [
            lp_imm(2, 1),                  // 0: R2 = 1
            lp_do(count, lp_rel(1, 4)),    // 1: DO (end 4)
            lp_add(0, 0, 2),               // 2
            lp_jump(0x1f, lp_rel(3, 8)),   // 3: JUMP 8 (out of line)
            lp_add(1, 1, 2),               // 4: loop end
            lp_jump(0x1f, lp_rel(5, 10)),  // 5: after the loop: JUMP 10
            0,                             // 6
            0,                             // 7
            lp_add(3, 3, 2),               // 8: out of line
            lp_jump(0x1f, lp_rel(9, 4)),   // 9: JUMP 4 (the loop's end)
            lp_jump(0x1f, 0),              // 10: spin
        ];
        let set = |e: &mut crate::Engine| lp_regs(e, &[(2, 1)]);
        for entries in [8, 9, 4, 0].map(|k| vec![lp_pc(k)]) {
            lp_check(&format!("dormant {count} {entries:?}"), &code, &entries, 90, &set);
        }
    }
}

/// An instruction the kernel cannot run (a write to a B register) inside a
/// loop started in the region, skipped on most iterations: the kernel leaves
/// there with the loop active and the interpreter finishes the iteration.
#[cfg(sharc_gen)]
#[test]
fn fast_stop_inside_a_loop_body_matches() {
    let code = [
        lp_imm(2, 1),                  // 0
        lp_do(6, lp_rel(1, 7)),        // 1: DO (end 7) 6 times
        lp_add(0, 0, 2),               // 2
        lp_sub(5, 5, 2),               // 3: R5 -= 1 (AZ)
        lp_2a(0x10, 0x01, 8, 8, 2),    // 4: IF NE R8 = R8 + R2 (keeps the flags)
        lp_imm(48, 0x25_f010),         // 5: B0 = ..., not lowered
        lp_add(1, 1, 2),               // 6
        lp_add(3, 3, 2),               // 7: end
        lp_jump(0x1f, 0),              // 8: spin
    ];
    let set = |e: &mut crate::Engine| lp_regs(e, &[(2, 1), (5, 3)]);
    for entries in [0, 2, 3, 6, 7].map(|k| vec![lp_pc(k)]) {
        lp_check(&format!("stop-in-body {entries:?}"), &code, &entries, 60, &set);
    }
}

/// Type 3d (byte and short accesses): loads and stores, post and pre
/// modify, a negative modifier, the sign-extended loads, a condition that
/// skips and executes, in straight code and in a loop.
#[cfg(sharc_gen)]
#[test]
fn fast_type_3d_matches_the_interpreter() {
    let at = |k: u32| LP_DATA + k;
    let setup = move |e: &mut crate::Engine| {
        lp_regs(
            e,
            &[
                (2, 1),
                (6, 0x1234_5678),
                (16, at(0x40)),  // I0
                (17, at(0x100)), // I1
                (18, at(0x1f0)), // I2
                (32, 1),         // M0
                (33, 2),         // M1
                (34, 0xffff_fffe), // M2 = -2
                (35, 0),         // M3
            ],
        )
    };
    // (l, x): byte, signed byte, short, signed short.
    for (l, x) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        let store_ok = x == 0;
        let mut code = vec![
            lp_3d(0x1f, false, true, 0, 0, 4, l, x),  // R4 = DM(I0, M0) post
            lp_3d(0x1f, false, false, 0, 1, 5, l, x), // R5 = DM(I0, M1) pre
            lp_3d(0x1f, false, true, 2, 2, 7, l, x),  // R7 = DM(I2, M2) post
            lp_3d(0x1f, false, false, 0, 3, 8, l, x), // R8 = DM(I0, M3) pre
            lp_add(9, 4, 5),                          // sets flags
            lp_3d(0x00, false, true, 0, 0, 10, l, x), // IF EQ R10 = DM(I0, M0)
            lp_3d(0x10, false, true, 0, 0, 11, l, x), // IF NE R11 = DM(I0, M0)
        ];
        if store_ok {
            code.extend([
                lp_3d(0x1f, true, true, 1, 0, 6, l, x),  // DM(I1, M0) = R6 post
                lp_3d(0x1f, true, false, 1, 1, 9, l, x), // DM(I1, M1) = R9 pre
                lp_3d(0x10, true, true, 1, 1, 6, l, x),  // IF NE DM(I1, M1) = R6
                lp_3d(0x00, true, true, 1, 1, 6, l, x),  // IF EQ DM(I1, M1) = R6
            ]);
        }
        code.push(lp_jump(0x1f, 0)); // spin
        let n = code.len() as u32;
        // Straight, from each instruction.
        let entries: Vec<u32> = (0..n).map(lp_pc).collect();
        let ran = lp_check(&format!("3d straight l{l} x{x}"), &code, &entries, n - 1, &setup);
        // Every 3d (and the compare between them) ran in the fast tier.
        assert_eq!(ran, u64::from(n - 1), "3d straight l{l} x{x}");

        // The same body in a loop of 6, with the pointers stepping.
        let body = &code[..code.len() - 1];
        // The DO is instruction 2, the body 3.., its last instruction the end.
        let mut prog = vec![
            lp_imm(0, 0),
            lp_imm(1, 0),
            lp_do(6, lp_rel(2, 2 + body.len() as i32)),
        ];
        prog.extend_from_slice(body);
        prog.push(lp_jump(0x1f, 0));
        let m = prog.len() as u32;
        let entries: Vec<u32> = (0..m).map(lp_pc).collect();
        lp_check(&format!("3d loop l{l} x{x}"), &prog, &entries, 120, &setup);
        lp_check(&format!("3d loop from 0 l{l} x{x}"), &prog, &[0], 120, &setup);
    }
}

/// Absolute short-word loads use byte addresses, zero-extend high-bit data,
/// and leave flags and DAG registers alone. Run only the load so the fast
/// instruction count proves that this transfer itself ran in the kernel.
#[cfg(sharc_gen)]
#[test]
fn fast_type_14d_matches_the_interpreter() {
    for addr in [0x25_590cu32, LP_DATA + 1, 0x8_0000, 0x25_fffe] {
        let code = [
            0x1a46_0000_0000 | u64::from(addr), // R6 = DM(addr) (sw)
            lp_jump(0x1f, 0),
        ];
        let setup = |e: &mut crate::Engine| {
            lp_regs(
                e,
                &[
                    (6, 0xdead_beef),
                    (16, 0x1234),
                    (32, 0xffff_ffff),
                    (118, 0x55),
                ],
            );
            let a = e.s.mem.canonical_of(addr);
            e.s.mem.write_byte(a, 0x17);
            e.s.mem.write_byte(a + 1, 0xe9);
        };
        assert_eq!(
            lp_check(&format!("14d {addr:#x}"), &code, &[0], 1, &setup),
            1
        );
        let mut fast = lp_engine(&code, &[0], true, &setup);
        assert_eq!(fast.step(1), 1);
        assert_eq!(fast.s.r[6], V::c(0xe917));
        assert_eq!(fast.s.r[118], V::c(0x55));
    }
}

/// A two-byte absolute window must decline at a page crossing or missing
/// backing byte. The interpreter then executes the load with the same state
/// and instruction boundary as a run without the fast tier.
#[cfg(sharc_gen)]
#[test]
fn fast_type_14d_window_fallback_matches_the_interpreter() {
    for (addr, complete) in [(0x25_ffffu32, true), (0x27_1000, false)] {
        let code = [0x1a46_0000_0000 | u64::from(addr), lp_jump(0x1f, 0)];
        let setup = |e: &mut crate::Engine| {
            lp_regs(e, &[(6, 0xdead_beef), (16, 0x1234), (32, 7), (118, 0x55)]);
            let a = e.s.mem.canonical_of(addr);
            e.s.mem.write_byte(a, 0x17);
            if complete {
                e.s.mem.write_byte(a + 1, 0xe9);
            }
        };
        assert_eq!(
            lp_compare(&format!("14d fallback {addr:#x}"), &code, &[0], 1, &setup),
            0,
        );
    }
}

// -- fast tier: a MODE1 write that enables interrupts ---------------------------------

/// `MODE1 = set(MODE1, mask)` (Type 18a, BOP set).
#[cfg(sharc_gen)]
fn lp_mode1_set(mask: u32) -> u64 {
    0x1402_0000_0000 | u64::from(mask)
}

/// `MODE1 = clear(MODE1, mask)` (Type 18a, BOP clear).
#[cfg(sharc_gen)]
fn lp_mode1_clear(mask: u32) -> u64 {
    0x1422_0000_0000 | u64::from(mask)
}

/// A MODE1 write that sets IRPTEN (or NESTM) while a source is already
/// latched and unmasked makes that interrupt deliverable at the next
/// instruction boundary: the interpreter enters it after the write, so a
/// kernel must not run on through the instructions that follow (the region
/// entry check sees IRPTEN clear and finds no candidate). Found by review of
/// the entry-state reads; no scenario of the DN2 sweep needs it.
#[cfg(sharc_gen)]
#[test]
fn fast_mode1_write_that_enables_interrupts_matches_the_interpreter() {
    let code = [
        lp_mode1_set(0x1000),    // 0: MODE1.IRPTEN on
        lp_add(1, 1, 2),         // 1: R1 += R2
        lp_add(3, 3, 2),         // 2: R3 += R2
        lp_add(4, 4, 2),         // 3: R4 += R2
        lp_jump(0x1f, 0),        // 4: spin
    ];
    // (MODE1, IRPTL, IMASK, IMASKP): the write enables a latched source;
    // the same with nesting (a source in service, a higher one latched); the
    // write changes nothing (already enabled, nothing latched).
    let cases: [(&str, u32, u32, u32, u32); 4] = [
        ("irpten", 0, 0x1000_0000, 0x1000_0000, 0),
        ("nestm", 0x1000, 0x1000_0000, 0x1000_0000, 0x2000_0000),
        // Nothing latched: the write still enables, so the kernel is refused
        // (the entry check cannot know a source will not latch in the region).
        ("nothing latched", 0, 0, 0x1000_0000, 0),
        // Already enabled and nothing latched: the write changes nothing and
        // the kernel runs.
        ("unchanged", 0x1000, 0, 0x1000_0000, 0),
    ];
    for (name, mode1, latch, mask, active) in cases {
        let code: Vec<u64> = if name == "nestm" {
            let mut c = code.to_vec();
            c[0] = lp_mode1_set(0x0800);
            c
        } else {
            code.to_vec()
        };
        let setup = move |e: &mut crate::Engine| {
            assert_eq!(e.set_option(9, 1), 0);
            e.s.cfg.stack_model = true;
            e.s.cfg.bank_model = true;
            e.s.cfg.refresh();
            e.s.mmr_put(0x30024, V::c(4));
            // The vector of the latched source (SFT0, level 28): a spin. The
            // vector table holds one 48-bit instruction per 6 bytes.
            let raw = lp_jump(0x1f, 0).to_le_bytes();
            for (k, b) in raw[..6].iter().enumerate() {
                e.s.mem.write_byte(0x2824_0000 + 28 * 4 * 6 + k as u32, *b);
            }
            lp_regs(e, &[(2, 1), (115, 0)]); // R2, MMASK
            e.s.r[114] = V::c(mode1 as Int);
            e.s.r[122] = V::c(latch as Int);
            e.s.r[123] = V::c(mask as Int);
            e.s.r[124] = V::c(active as Int);
        };
        // Regions at every instruction, and one only at the write.
        let all: Vec<u32> = (0..5).map(lp_pc).collect();
        lp_compare(&format!("mode1 enable {name}"), &code, &all, 12, &setup);
        let ran = lp_compare(&format!("mode1 enable {name}, at the write"), &code, &[0], 12, &setup);
        // The enabling write is never lowered (the generated core of a
        // firmware image may run its own blocks at these PCs instead).
        if name != "unchanged" {
            assert_eq!(ran, 0, "{name}: the fast tier ran the enabling write");
        }
    }
}

/// A MODE1 write inside a DO loop body that changes a bit the body depends on
/// (here PEYEN: from the second iteration the whole body runs in SIMD). The
/// kernel lowers the body once under the MODE1 of the loop head, so the back
/// edge must meet the head under the same MODE1: a region whose body leaves
/// MODE1 different is refused. Found by `fast_diff cfg --simple` (cases 180
/// and 275 of the default seed): the kernel ran every iteration under the
/// first iteration's MODE1.
#[cfg(sharc_gen)]
#[test]
fn fast_mode1_write_in_a_loop_body_is_not_carried_to_the_next_iteration() {
    const PEYEN: u32 = 1 << 21;
    let code = [
        lp_do(4, lp_rel(0, 3)), // 0: DO (end 3) 4 times
        lp_add(3, 3, 2),        // 1: R3 += R2 (and S3 += S2 once SIMD)
        lp_mode1_set(PEYEN),    // 2: SIMD on
        lp_add(1, 1, 2),        // 3: R1 += R2 (and S1 += S2)  (end)
        lp_mode1_clear(PEYEN),  // 4: SIMD off
        lp_jump(0x1f, 0),       // 5: spin
    ];
    let set = |e: &mut crate::Engine| {
        for code in 80..96 {
            e.s.r[code] = V::c(0);
        }
        lp_regs(e, &[(2, 1), (82, 1)]); // R2, S2
    };
    let all: Vec<u32> = (0..6).map(lp_pc).collect();
    lp_compare("mode1 in a loop body, all entries", &code, &all, 40, &set);
    for entries in [vec![lp_pc(0)], vec![lp_pc(1)], vec![lp_pc(3)]] {
        lp_compare(&format!("mode1 in a loop body {entries:?}"), &code, &entries, 40, &set);
    }
    // A body that leaves MODE1 as it found it still runs in a region.
    let steady = [
        lp_do(4, lp_rel(0, 3)),
        lp_add(3, 3, 2),
        lp_mode1_set(0),
        lp_add(1, 1, 2),
        lp_jump(0x1f, 0),
    ];
    lp_check("mode1 steady in a loop body", &steady, &[lp_pc(0)], 20, &set);
}

/// `Rn = FIX Fx` (compute cu 0, opcode 0xc9).
#[cfg(sharc_gen)]
fn lp_fix(rn: u64, rx: u64) -> u64 {
    lp_2a(0x1f, 0x0c9, rn, rx, 0)
}

/// A kernel reads MODE1 as the region itself left it: a MODE1 write earlier
/// in the region changes `fix` (TRUNCATE), the signed conditions (ALUSAT) and
/// the circular post-modify (CBUFEN), and the entry value of the bit is not
/// the one that applies. Found with `sf-del-time-u20` (DN2 1.11 `0xb8b4de`:
/// `MODE1 = set(MODE1, TRUNCATE)` then `fix`).
#[cfg(sharc_gen)]
#[test]
fn fast_reads_mode1_as_the_region_wrote_it() {
    const TRUNCATE: u32 = 1 << 15;
    const ALUSAT: u32 = 1 << 13;
    const CBUFEN: u32 = 1 << 24;
    let at = |k: u32| LP_DATA + k;
    // (name, MODE1 at entry, code, setup): each also with a region at every
    // instruction.
    type Setup = Box<dyn Fn(&mut crate::Engine)>;
    let mode = |m: u32| -> Setup {
        Box::new(move |e| {
            lp_regs(e, &[(1, 0x4030_0000), (4, 0x7fff_ffff), (2, 1)]); // F1 = 2.75
            e.s.r[114] = V::c(m as Int);
        })
    };
    let cases: Vec<(&str, Vec<u64>, Setup)> = vec![
        (
            "truncate on in the region",
            vec![lp_mode1_set(TRUNCATE), lp_fix(0, 1), lp_jump(0x1f, 0)],
            mode(0),
        ),
        (
            "truncate off in the region",
            vec![lp_mode1_clear(TRUNCATE), lp_fix(0, 1), lp_jump(0x1f, 0)],
            mode(TRUNCATE),
        ),
        (
            // 0x7fffffff + 1 overflows: AN and AV set; LT = AN ^ AV unless
            // ALUSAT, when it is AN.
            "alusat on in the region",
            vec![
                lp_mode1_set(ALUSAT),
                lp_add(5, 4, 2),
                lp_2a(0x01, 0x01, 6, 6, 2), // IF LT R6 = R6 + R2
                lp_jump(0x1f, 0),
            ],
            mode(0),
        ),
        (
            "alusat off in the region",
            vec![
                lp_mode1_clear(ALUSAT),
                lp_add(5, 4, 2),
                lp_2a(0x01, 0x01, 6, 6, 2),
                lp_jump(0x1f, 0),
            ],
            mode(ALUSAT),
        ),
        (
            // A post-modify that crosses the end of a 16-byte circular buffer.
            "cbufen off in the region",
            vec![
                lp_mode1_clear(CBUFEN),
                lp_3d(0x1f, false, true, 1, 0, 6, 0, 0),
                lp_jump(0x1f, 0),
            ],
            Box::new(move |e| {
                lp_regs(
                    e,
                    &[(17, at(0x40) + 12), (32, 8), (49, 16), (65, at(0x40))],
                );
                e.s.r[114] = V::c(CBUFEN as Int);
            }),
        ),
        (
            "cbufen on in the region",
            vec![
                lp_mode1_set(CBUFEN),
                lp_3d(0x1f, false, true, 1, 0, 6, 0, 0),
                lp_jump(0x1f, 0),
            ],
            Box::new(move |e| {
                lp_regs(
                    e,
                    &[(17, at(0x40) + 12), (32, 8), (49, 16), (65, at(0x40))],
                );
                e.s.r[114] = V::c(0);
            }),
        ),
    ];
    for (name, code, setup) in &cases {
        let n = code.len() as u32;
        let all: Vec<u32> = (0..n).map(lp_pc).collect();
        lp_compare(name, code, &all, 3 * (n - 1), setup.as_ref());
        lp_compare(&format!("{name}, region at the write"), code, &[0], 3 * (n - 1), setup.as_ref());
    }
}

/// `I(dst) = MODIFY(I(src), imm)` (Type 19a, unscaled; `dst = src ^ idis`).
#[cfg(sharc_gen)]
fn lp_modify_imm(src: u64, idis: u64, imm: u32) -> u64 {
    0x1600_0000_0000 | (idis << 35) | (src << 32) | u64::from(imm)
}

/// The circular wrap of MODIFY follows L and B as the region left them (the
/// interpreter wraps whenever L is nonzero): a region that sets L1/B1 and
/// then modifies I1 must not use the entry L1 = 0 (`require_linear`).
#[cfg(sharc_gen)]
#[test]
fn fast_modify_follows_l_written_in_the_region() {
    let at = |k: u32| LP_DATA + k;
    let code = [
        lp_imm(49, 16),               // 0: L1 = 16
        lp_imm(65, at(0x40)),         // 1: B1 = buffer
        lp_modify_imm(1, 0, 8),       // 2: I1 = MODIFY(I1, 8): wraps past B1 + 16
        lp_jump(0x1f, 0),             // 3: spin
    ];
    let setup = move |e: &mut crate::Engine| lp_regs(e, &[(17, at(0x40) + 12)]);
    let all: Vec<u32> = (0..4).map(lp_pc).collect();
    lp_compare("modify after L write", &code, &all, 9, &setup);
    lp_check("modify after L write, region at the write", &code, &[0], 9, &setup);
}

/// Synthetic dispatch and tier: exercise fallback policy without generated firmware.
struct FallbackProbe {
    only: bool,
}

impl crate::fast::FastTier for FallbackProbe {
    fn wants(&self, pc: u32) -> bool {
        pc <= 3
    }

    fn fallback_only(&self, _pc: u32) -> bool {
        assert!(self.only, "ordinary tier must not consult fallback policy");
        self.only
    }

    fn has_fallback(&self) -> bool {
        self.only
    }

    fn exclusively_fallback(&self) -> bool {
        self.only
    }

    fn run(&mut self, s: &mut St, _pc: u32) -> Option<u32> {
        s.r[1] = V::c(1);
        s.icount = s.limit;
        Some(crate::EXIT_BUDGET)
    }

    fn run_chained(&mut self, s: &mut St, pc: u32) -> Option<u32> {
        self.run(s, pc)
    }
}

fn fallback_probe_block(s: &mut St) -> u32 {
    s.r[0] = V::c(1);
    s.icount = s.limit;
    crate::EXIT_BUDGET
}

fn fallback_probe_engine(code_ok: bool, barred: Option<Vec<u32>>) -> crate::Engine {
    let mut e = crate::Engine::new(Mem::new());
    e.s.pc_sw = 0;
    e.s.r[0] = V::c(0);
    e.s.r[1] = V::c(0);
    e.s.runtime_decode = true;
    e.use_blocks = true;
    e.dispatch = crate::Dispatch::new(&[(0, fallback_probe_block)], &[0]);
    e.code_known = true;
    e.code_checked = e.s.mem.code_gen;
    e.code_ok = code_ok;
    e.code_barred = barred;
    e.set_fast(Some(Box::new(FallbackProbe { only: true })));
    e
}

#[test]
fn fallback_only_requires_a_barred_aot_entry() {
    for (code_ok, barred, expected) in [
        (true, Some(vec![]), false),
        (false, Some(vec![0]), true),
        (false, Some(vec![3]), false),
        (false, None, true),
    ] {
        let mut e = fallback_probe_engine(code_ok, barred);
        assert_eq!(e.fast_allowed(0), expected);
        assert!(!e.fast_allowed(3), "missing dispatch entry");
        assert!(!e.fast_allowed(4), "unregistered PC");
        let plan = e.block_plan(10).expect("AOT or fallback plan");
        assert_eq!(plan.0.is_none(), expected);
        e.step(10);
        assert_eq!(e.s.r[1], V::c(i128::from(expected)));
        assert_eq!(e.s.r[0], V::c(i128::from(!expected)));
        e.use_blocks = false;
        assert!(!e.fast_allowed(0), "fallback requires AOT enabled");
        assert!(e.block_plan(20).is_none());
    }
}

#[test]
fn fallback_continuation_cannot_bypass_code_policy() {
    for barred in [Some(vec![]), Some(vec![0]), None] {
        let expected = barred.as_ref().is_none_or(|b| b.contains(&0));
        let mut e = fallback_probe_engine(false, barred);
        e.s.limit = 10;
        let exit = e.fast_continue(10, Some(crate::EXIT_NEXT));
        assert_eq!(e.s.r[1], V::c(i128::from(expected)));
        assert_eq!(
            exit,
            Some(if expected {
                crate::EXIT_BUDGET
            } else {
                crate::EXIT_NEXT
            })
        );
    }
    let mut e = fallback_probe_engine(false, None);
    e.use_blocks = false;
    assert_eq!(
        e.fast_continue(10, Some(crate::EXIT_NEXT)),
        Some(crate::EXIT_NEXT)
    );
    assert_eq!(e.s.r[1], V::c(0));
}

#[test]
fn ordinary_fast_regions_keep_fast_only_and_stock_behavior() {
    for use_blocks in [false, true] {
        let mut e = fallback_probe_engine(true, Some(vec![]));
        e.set_fast(Some(Box::new(FallbackProbe { only: false })));
        e.use_blocks = use_blocks;
        assert!(e.fast_allowed(0));
        e.step(10);
        assert_eq!(e.s.r[1], V::c(1));
        assert_eq!(e.s.r[0], V::c(0));
    }
}

#[test]
fn fallback_registration_preserves_healthy_chain_stops() {
    use crate::fast::FastTier;
    let mut f = crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), &[1])
        .with_clusters(&[2])
        .with_fallback(&[1, 2, 3, 3]);
    let mut s = St::new(Mem::new());
    assert!(f.sync_chain_stops(&mut s));
    assert!(s.chain_stops_at(1));
    assert!(s.chain_stops_at(2));
    assert!(!s.chain_stops_at(3));
    assert!(!f.fallback_only(1));
    assert!(!f.fallback_only(2));
    assert!(f.fallback_only(3));
    assert!(f.wants(3));
}

#[cfg(not(all(sharc_gen, sharc_image)))]
#[test]
fn fallback_continuation_rechecks_code_freshness() {
    let mut e = fallback_probe_engine(false, None);
    assert!(e.fast_allowed(0));
    // A kernel's code store changes the generation; without an image the
    // fresh verifier reports no protected code, superseding the stale failure.
    e.s.mem.code_gen += 1;
    assert_eq!(
        e.fast_continue(10, Some(crate::EXIT_NEXT)),
        Some(crate::EXIT_NEXT)
    );
    assert_eq!(e.s.r[1], V::c(0));
    assert!(e.code_ok);
}

#[test]
fn ordinary_tier_dispatch_preserves_follow_side_effects() {
    struct FollowProbe;
    impl crate::fast::FastTier for FollowProbe {
        fn wants(&self, _pc: u32) -> bool {
            false
        }
        fn run(&mut self, s: &mut St, _pc: u32) -> Option<u32> {
            // A tier may flush its follow queue here, even before wants sees it.
            s.r[1] = V::c(1);
            None
        }
    }
    let mut e = fallback_probe_engine(true, Some(vec![]));
    e.set_fast(Some(Box::new(FollowProbe)));
    e.step(10);
    assert_eq!(e.s.r[0], V::c(1));
    assert_eq!(e.s.r[1], V::c(1));
}

#[test]
fn fallback_registration_preserves_auto_only_chaining() {
    use crate::fast::FastTier;
    for roots in [&[][..], &[3][..]] {
        let mut f = crate::fast::FastEngine::new(Box::new(crate::fast::interp::InterpBackend), &[])
            .with_fallback(roots)
            .with_auto(2, 0, false);
        assert!(!f.caps_chain());
        let mut s = St::new(Mem::new());
        assert!(f.sync_chain_stops(&mut s));
        assert!(!s.chain_stops_at(3));
        f.note(1);
        f.note(1);
        assert!(f.wants(1));
        assert!(!f.fallback_only(1));
        assert!(!f.caps_chain());
        assert!(f.sync_chain_stops(&mut s));
        assert!(!s.chain_stops_at(1));
        assert!(!s.chain_stops_at(3));
    }
}

#[test]
fn healthy_fallback_only_tier_is_inactive_until_code_is_barred() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct ActivityProbe(Arc<AtomicUsize>);
    impl crate::fast::FastTier for ActivityProbe {
        fn has_fallback(&self) -> bool {
            true
        }
        fn exclusively_fallback(&self) -> bool {
            true
        }
        fn wants(&self, _pc: u32) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            true
        }
        fn fallback_only(&self, _pc: u32) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            true
        }
        fn run(&mut self, s: &mut St, _pc: u32) -> Option<u32> {
            self.0.fetch_add(1, Ordering::Relaxed);
            s.r[1] = V::c(1);
            s.icount = s.limit;
            Some(crate::EXIT_BUDGET)
        }
        fn sync_chain_stops(&mut self, s: &mut St) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            s.chain_stop_add(7);
            true
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = fallback_probe_engine(true, Some(vec![]));
    e.set_fast(Some(Box::new(ActivityProbe(calls.clone()))));
    e.step(10);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(e.s.r[0], V::c(1));
    assert_eq!(e.s.r[1], V::c(0));
    assert!(!e.s.chain_stops_at(7));
    assert_eq!(
        e.fast_continue(20, Some(crate::EXIT_NEXT)),
        Some(crate::EXIT_NEXT)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    // Simulate a later verification detecting a patched AOT entry.
    e.code_ok = false;
    e.code_barred = Some(vec![0]);
    e.step(10);
    assert!(calls.load(Ordering::Relaxed) > 0);
    assert_eq!(e.s.r[1], V::c(1));
}

#[test]
fn ordinary_auto_and_follow_tiers_remain_active_with_healthy_code() {
    use crate::fast::{FastEngine, FastTier, interp::InterpBackend};
    let fallback = || FastEngine::new(Box::new(InterpBackend), &[]).with_fallback(&[0]);
    assert!(fallback().has_fallback());
    assert!(fallback().exclusively_fallback());
    for f in [
        fallback().with_follow(true),
        fallback().with_auto(2, 0, false),
        fallback().with_clusters(&[1]),
        FastEngine::new(Box::new(InterpBackend), &[1]).with_fallback(&[0]),
    ] {
        assert!(f.has_fallback());
        assert!(!f.exclusively_fallback());
        let mut e = fallback_probe_engine(true, Some(vec![]));
        e.set_fast(Some(Box::new(f)));
        assert!(e.block_plan(10).unwrap().1.fast_active);
    }
}

#[test]
fn profile_writer_emits_one_file_set_only_when_maps_are_on() {
    let dir = std::env::temp_dir().join(format!("sharc-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let names = |dir: &std::path::Path| {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    };
    let mut engine = crate::Engine::new(Mem::new());
    engine.write_profile(&dir.join("p"));
    assert!(names(&dir).is_empty());
    engine.set_option(26, 1);
    engine.write_profile(&dir.join("p"));
    let files = names(&dir);
    assert_eq!(files.len(), 3);
    assert!(files.iter().any(|f| f.ends_with(".entries")));
    assert!(files.iter().any(|f| f.ends_with(".trans")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ddr_monitor_captures_cpu_attempts_before_synthesis_and_journal() {
    use crate::mem::DdrAccessKind;
    let a = 0x8001_0000;
    let mut s = St::new(Mem::new());
    s.cfg.explicit_memory_model = true;
    s.mem.set_ddr_monitor(Some((a, a + 4))).unwrap();
    assert_eq!(
        bnd::_dm_read(&s, VI::I(a as Int), 4, false, false).unwrap(),
        Some(V::c(0))
    );
    assert_eq!(
        s.mem.first_ddr_access().unwrap().kind,
        DdrAccessKind::CpuRead
    );
    for k in 0..4 {
        s.mem.write_byte(a + k, 0);
    }
    for nolog in [false, true] {
        s.mem.clear_ddr_access();
        let result = if nolog {
            bnd::_dm_write_nolog_b(&mut s, VI::I(a as Int), 4, V::c(0), false)
        } else {
            bnd::_dm_write_b(&mut s, VI::I(a as Int), 4, V::c(0), false)
        };
        assert_eq!(result.unwrap(), true);
        assert_eq!(
            s.mem.first_ddr_access().unwrap().kind,
            DdrAccessKind::CpuWrite
        );
        assert_eq!(s.mem.read_le(a, 4), 0);
    }
    s.mem.clear_ddr_access();
    assert_eq!(
        bnd::_dm_read_b(&s, VI::I(a as Int), 4, false, false).unwrap(),
        Some(V::c(0))
    );
    assert_eq!(
        s.mem.first_ddr_access().unwrap().kind,
        DdrAccessKind::CpuRead
    );
}

#[test]
fn ddr_monitor_known_attempts_survive_symbolic_value_and_model_declines() {
    use crate::mem::DdrAccessKind;
    let a = 0x8001_0000;
    let mut s = St::new(Mem::new());
    s.mem.set_ddr_monitor(Some((a, a + 4))).unwrap();
    assert!(!bnd::_dm_write(&mut s, VI::I(a as Int), 4, V::UNK, false).unwrap());
    assert_eq!(
        s.mem.first_ddr_access().unwrap().kind,
        DdrAccessKind::CpuWrite
    );
    assert!(!s.mem.present(a));
    s.mem.clear_ddr_access();
    s.cfg.has_concrete = false;
    assert_eq!(
        bnd::_dm_read_full(&s, VI::I(a as Int), 4, false, false).unwrap(),
        None
    );
    assert_eq!(
        s.mem.first_ddr_access().unwrap().kind,
        DdrAccessKind::CpuRead
    );
    s.mem.clear_ddr_access();
    assert!(!bnd::_dm_write_full(&mut s, VI::I(a as Int), 3, V::c(1), false).unwrap());
    assert_eq!(s.mem.first_ddr_access(), None);
    assert!(!bnd::_dm_write_full(&mut s, VI::V(V::UNK), 4, V::c(1), false).unwrap());
    assert_eq!(s.mem.first_ddr_access(), None);
}

#[test]
fn ddr_monitor_disables_idle_skip_and_discards_transition_evidence() {
    let mut e = crate::Engine::new(Mem::new());
    e.s.pc_sw = 100;
    e.idle_visit(100, 1000);
    assert!(e.idle_snap.is_some());
    assert!(e.s.probe.is_some());
    e.set_ddr_monitor(Some((0x8000_0000, 0x8000_0004))).unwrap();
    assert!(e.idle_snap.is_none());
    assert!(e.s.probe.is_none());
    e.idle_visit(100, 1000);
    assert!(e.idle_snap.is_none());
    assert_eq!(e.idle_stats.skips, 0);
    e.set_ddr_monitor(None).unwrap();
    assert!(e.idle_snap.is_none());
    e.idle_visit(100, 1000);
    assert!(e.idle_snap.is_some());
    // Direct memory configuration is also detected at the next idle visit,
    // even when activation and removal happen between visits.
    e.s.mem
        .set_ddr_monitor(Some((0x8000_0000, 0x8000_0004)))
        .unwrap();
    e.s.mem.set_ddr_monitor(None).unwrap();
    e.idle_visit(100, 1000);
    assert_eq!(e.idle_stats.skips, 0);
}
