//! The register contract rules (`dksharc::rules`) on small hand-made functions: one block that ends in a return, so each case shows
//! one rule without an assembler.

use dksharc::rules::check;
use dksharc::*;

const BASE: u32 = 0x1000;

fn insn(pc: u32, reads: &[&str], writes: &[&str]) -> Insn {
    Insn {
        pc,
        parcels: 1,
        form: "x".to_string(),
        import: None,
        mem: vec![],
        defuse: DefUse {
            reads: reads.iter().map(|r| r.to_string()).collect(),
            writes: writes.iter().map(|r| r.to_string()).collect(),
            ..DefUse::default()
        },
        imm: None,
    }
}

/// A MODIFY of I7 by an immediate (form `19a_scaled`, as `MODIFY(I7, imm) (NW)`).
fn modify_i7(pc: u32, imm: i64) -> Insn {
    let mut i = insn(pc, &["I7"], &["I7"]);
    i.form = "19a_scaled".to_string();
    i.imm = Some(imm);
    i
}

/// `RFRAME`: `I7 = I6; I6 = DM(0, I6)`.
fn rframe(pc: u32) -> Insn {
    let mut i = insn(pc, &["I6"], &["I7", "I6"]);
    i.form = "25c_rframe".to_string();
    i
}

/// `CJUMP`: `R2 = I6; I6 = I7`.
fn cjump(pc: u32) -> Insn {
    let mut i = insn(pc, &["I7"], &["R2", "I6"]);
    i.form = "25a_direct".to_string();
    i
}

/// The problems of a function whose last three instructions are a return jump and its two delay slots: (rule, register).
fn problems_delayed_return(insns: Vec<Insn>) -> Vec<(&'static str, String)> {
    let n = insns.len() as u32;
    let f = Function {
        name: "f".to_string(),
        entry: BASE,
        end: BASE + n,
        global: true,
        origin: vec![],
        complete: true,
        blocks: vec![Block {
            start: BASE,
            end: BASE + n,
            term_pc: Some(BASE + n - 3),
            term: Terminator::Return {
                conditional: false,
                delayed: true,
            },
            delay_slots: vec![BASE + n - 2, BASE + n - 1],
            succ: vec![],
        }],
        effects: FunctionEffects::default(),
    };
    let a = Analysis {
        insns,
        functions: vec![f.clone()],
        ..Analysis::default()
    };
    check(&a, &f).into_iter().map(|p| (p.rule, p.reg)).collect()
}

/// An access through I7 (the stack) with a post-modify by M6 or M7.
fn stack_access(pc: u32, dir: Direction, data: &str, m: u8) -> Insn {
    let mreg = format!("M{m}");
    let (reads, writes): (Vec<&str>, Vec<&str>) = match dir {
        Direction::Store => (vec![data, "I7", mreg.as_str()], vec!["I7"]),
        Direction::Load => (vec!["I7", mreg.as_str()], vec![data, "I7"]),
    };
    let mut i = insn(pc, &reads, &writes);
    i.mem.push(MemOp {
        space: Space::Dm,
        dir,
        mode: Mode::PostModify,
        access: Access::Normal,
        i: Some(7),
        m: Some(m),
        imm: None,
        addr: None,
        import: None,
    });
    i
}

fn push(pc: u32, data: &str) -> Insn {
    stack_access(pc, Direction::Store, data, 7)
}

fn pop(pc: u32, data: &str) -> Insn {
    stack_access(pc, Direction::Load, data, 6)
}

/// The problems of a function made of `insns`, then a return at the next pc: (rule, register).
fn problems(mut insns: Vec<Insn>) -> Vec<(&'static str, String)> {
    let ret = BASE + insns.len() as u32;
    insns.push(insn(ret, &[], &[]));
    let f = Function {
        name: "f".to_string(),
        entry: BASE,
        end: ret + 1,
        global: true,
        origin: vec![],
        complete: true,
        blocks: vec![Block {
            start: BASE,
            end: ret + 1,
            term_pc: Some(ret),
            term: Terminator::Return {
                conditional: false,
                delayed: false,
            },
            delay_slots: vec![],
            succ: vec![],
        }],
        effects: FunctionEffects::default(),
    };
    let a = Analysis {
        insns,
        functions: vec![f.clone()],
        ..Analysis::default()
    };
    check(&a, &f).into_iter().map(|p| (p.rule, p.reg)).collect()
}

fn one(rule: &'static str, reg: &str) -> Vec<(&'static str, String)> {
    vec![(rule, reg.to_string())]
}

#[test]
fn write_to_a_never_write_register() {
    let p = problems(vec![insn(BASE, &["R0"], &["M6"])]);
    assert_eq!(p, one("never_write", "M6"));
}

#[test]
fn write_to_the_entry_register_is_free() {
    assert_eq!(problems(vec![insn(BASE, &["R0"], &["I12"])]), vec![]);
}

#[test]
fn balanced_push_and_pop_on_the_stack() {
    let p = problems(vec![push(BASE, "R0"), pop(BASE + 1, "R0")]);
    assert_eq!(p, vec![]);
}

#[test]
fn loading_the_stack_pointer_is_a_problem() {
    let p = problems(vec![insn(BASE, &["R0"], &["I7"])]);
    assert_eq!(p, one("stack", "I7"));
}

#[test]
fn unbalanced_pushes_at_a_return() {
    let p = problems(vec![
        push(BASE, "R0"),
        push(BASE + 1, "R1"),
        pop(BASE + 2, "R1"),
    ]);
    assert_eq!(p, one("stack", "I7"));
}

#[test]
fn frame_pointer_saved_by_push_and_restored_by_pop() {
    let p = problems(vec![
        push(BASE, "I6"),
        insn(BASE + 1, &["I7"], &["I6"]),
        pop(BASE + 2, "I6"),
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn mode1_saved_in_a_data_register_and_restored() {
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R0"]),
        insn(BASE + 1, &["MODE1"], &["MODE1"]),
        insn(BASE + 2, &["R0"], &["MODE1"]),
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn mode1_changed_and_not_restored() {
    let p = problems(vec![insn(BASE, &["MODE1"], &["MODE1"])]);
    assert_eq!(p, one("keep", "MODE1"));
}

#[test]
fn mode1_copy_is_lost_when_the_data_register_is_overwritten() {
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R0"]),
        insn(BASE + 1, &["MODE1"], &["MODE1"]),
        insn(BASE + 2, &["R2"], &["R0"]),
        insn(BASE + 3, &["R0"], &["MODE1"]),
    ]);
    assert_eq!(p, one("keep", "MODE1"));
}

#[test]
fn instruction_with_uncertain_effects() {
    let mut i = insn(BASE, &[], &[]);
    i.defuse.uncertain.push("not modelled".to_string());
    assert_eq!(problems(vec![i]), one("uncertain", "not modelled"));
}

#[test]
fn frame_exit_restores_the_depth_of_the_frame_setup() {
    let p = problems(vec![
        push(BASE, "I6"),
        insn(BASE + 1, &["I7"], &["I6"]),
        push(BASE + 2, "R0"),
        push(BASE + 3, "R1"),
        insn(BASE + 4, &["I6"], &["I7"]),
        pop(BASE + 5, "I6"),
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn dk_enter_and_dk_leave_are_balanced() {
    // DK_ENTER(1), a body, DK_LEAVE: JUMP (DB), a reload, RFRAME.
    let p = problems_delayed_return(vec![
        modify_i7(BASE, -1),
        insn(BASE + 1, &["R0"], &[]),
        insn(BASE + 2, &[], &[]),
        insn(BASE + 3, &[], &[]),
        rframe(BASE + 4),
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn dk_call_between_modifies_of_the_stack_pointer_is_balanced() {
    // MODIFY(I7, -1); CJUMP (DB) with two delayed pushes; MODIFY(I7, 1); return.
    let insns = vec![
        insn(BASE, &[], &[]),
        modify_i7(BASE + 1, -1),
        cjump(BASE + 2),
        push(BASE + 3, "R2"),
        push(BASE + 4, "R0"),
        modify_i7(BASE + 5, 1),
        insn(BASE + 6, &[], &[]),
    ];
    let f = Function {
        name: "f".to_string(),
        entry: BASE,
        end: BASE + 7,
        global: true,
        origin: vec![],
        complete: true,
        blocks: vec![
            Block {
                start: BASE,
                end: BASE + 5,
                term_pc: Some(BASE + 2),
                term: Terminator::Call {
                    conditional: false,
                    delayed: true,
                },
                delay_slots: vec![BASE + 3, BASE + 4],
                succ: vec![Edge {
                    to: BASE + 5,
                    kind: EdgeKind::ReturnSite,
                }],
            },
            Block {
                start: BASE + 5,
                end: BASE + 7,
                term_pc: Some(BASE + 6),
                term: Terminator::Return {
                    conditional: false,
                    delayed: false,
                },
                delay_slots: vec![],
                succ: vec![],
            },
        ],
        effects: FunctionEffects::default(),
    };
    let a = Analysis {
        insns,
        functions: vec![f.clone()],
        ..Analysis::default()
    };
    assert_eq!(check(&a, &f), vec![]);
}

#[test]
fn modify_of_the_stack_pointer_without_its_counterpart_is_a_problem() {
    let p = problems(vec![insn(BASE, &[], &[]), modify_i7(BASE + 1, -1)]);
    assert_eq!(p, one("stack", "I7"));
}

#[test]
fn rframe_without_a_frame_setup_is_a_problem() {
    let p = problems(vec![insn(BASE, &[], &[]), rframe(BASE + 1)]);
    assert_eq!(p, one("stack", "I7"));
}

/// The problems of a function made of `insns` and `blocks` as given: (rule, register).
fn run(insns: Vec<Insn>, blocks: Vec<Block>) -> Vec<(&'static str, String)> {
    let end = insns.last().map_or(BASE, |i| i.pc + 1);
    let f = Function {
        name: "f".to_string(),
        entry: BASE,
        end,
        global: true,
        origin: vec![],
        complete: true,
        blocks,
        effects: FunctionEffects::default(),
    };
    let a = Analysis {
        insns,
        functions: vec![f.clone()],
        ..Analysis::default()
    };
    check(&a, &f).into_iter().map(|p| (p.rule, p.reg)).collect()
}

fn block(start: u32, end: u32, term_pc: Option<u32>, term: Terminator, succ: Vec<Edge>) -> Block {
    Block {
        start,
        end,
        term_pc,
        term,
        delay_slots: vec![],
        succ,
    }
}

#[test]
fn simd_companion_written_after_peyen_is_a_keep_problem() {
    let mut set = insn(BASE + 1, &["MODE1"], &["MODE1"]);
    set.defuse.mode1 = Some(Mode1Effect {
        op: Mode1Op::Set,
        mask: 1 << 21,
        value: None,
    });
    let mut w = insn(BASE + 2, &["R1"], &["R0"]);
    w.defuse.simd_writes = vec!["S0".to_string()];
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R5"]),
        set,
        w,
        insn(BASE + 3, &["R5"], &["MODE1"]),
    ]);
    assert_eq!(p, one("keep", "S0"));
}

/// A MODE1 instruction with the effect `op` on `mask` (`BIT CLR MODE1 0x1000`).
fn mode1_bit(pc: u32, op: Mode1Op, mask: u32) -> Insn {
    let mut i = insn(pc, &["MODE1"], &["MODE1"]);
    i.defuse.mode1 = Some(Mode1Effect {
        op,
        mask,
        value: None,
    });
    i
}

/// `MODE1 = Rn`.
fn mode1_from(pc: u32, rn: &str) -> Insn {
    let mut i = insn(pc, &[rn], &["MODE1"]);
    i.defuse.mode1 = Some(Mode1Effect {
        op: Mode1Op::Full,
        mask: u32::MAX,
        value: None,
    });
    i
}

#[test]
fn repeated_save_clear_restore_windows_of_mode1_are_fine() {
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R13"]),
        mode1_bit(BASE + 1, Mode1Op::Clear, 0x1000),
        mode1_from(BASE + 2, "R13"),
        insn(BASE + 3, &["MODE1"], &["R13"]),
        mode1_bit(BASE + 4, Mode1Op::Clear, 0x1000),
        mode1_from(BASE + 5, "R13"),
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn simd_companion_after_the_restore_of_mode1_is_not_a_write() {
    let mut w = insn(BASE + 3, &["R1"], &["R0"]);
    w.defuse.simd_writes = vec!["S0".to_string()];
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R13"]),
        mode1_bit(BASE + 1, Mode1Op::Clear, 0x1000),
        mode1_from(BASE + 2, "R13"),
        w,
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn peyen_window_is_a_keep_problem_on_the_simd_register_only() {
    let mut w = insn(BASE + 2, &["R1"], &["R0"]);
    w.defuse.simd_writes = vec!["S0".to_string()];
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R13"]),
        mode1_bit(BASE + 1, Mode1Op::Set, 1 << 21),
        w,
        mode1_from(BASE + 3, "R13"),
    ]);
    assert_eq!(p, one("keep", "S0"));
}

#[test]
fn mode1_pushed_changed_and_popped_is_restored() {
    let mut w = insn(BASE + 3, &["R1"], &["R0"]);
    w.defuse.simd_writes = vec!["S0".to_string()];
    let p = problems(vec![
        push(BASE, "MODE1"),
        mode1_bit(BASE + 1, Mode1Op::Set, 1 << 21),
        pop(BASE + 2, "MODE1"),
        w,
    ]);
    assert_eq!(p, vec![]);
}

#[test]
fn mode1_bit_cleared_and_not_restored_is_a_keep_problem() {
    let p = problems(vec![
        insn(BASE, &["MODE1"], &["R13"]),
        mode1_bit(BASE + 1, Mode1Op::Clear, 0x1000),
    ]);
    assert_eq!(p, one("keep", "MODE1"));
}

#[test]
fn mode1_loaded_from_a_register_that_never_copied_it_is_a_keep_problem() {
    let p = problems(vec![mode1_from(BASE, "R5")]);
    assert_eq!(p, one("keep", "MODE1"));
}

#[test]
fn simd_companion_before_peyen_is_not_a_write() {
    let mut w = insn(BASE, &["R1"], &["R0"]);
    w.defuse.simd_writes = vec!["S0".to_string()];
    assert_eq!(problems(vec![w]), vec![]);
}

#[test]
fn merge_with_different_depths_is_uncertain() {
    let ret = Terminator::Return {
        conditional: false,
        delayed: false,
    };
    let jump = Terminator::Jump {
        target: Some(BASE + 3),
        conditional: true,
        delayed: false,
    };
    let edge = |to, kind| Edge { to, kind };
    let p = run(
        vec![
            push(BASE, "R0"),
            insn(BASE + 1, &[], &[]),
            push(BASE + 2, "R1"),
            insn(BASE + 3, &[], &[]),
        ],
        vec![
            block(
                BASE,
                BASE + 2,
                Some(BASE + 1),
                jump,
                vec![
                    edge(BASE + 3, EdgeKind::Taken),
                    edge(BASE + 2, EdgeKind::Fall),
                ],
            ),
            block(
                BASE + 2,
                BASE + 3,
                None,
                Terminator::Fall,
                vec![edge(BASE + 3, EdgeKind::Fall)],
            ),
            block(BASE + 3, BASE + 4, Some(BASE + 3), ret, vec![]),
        ],
    );
    assert_eq!(p, one("uncertain", "stack depth differs at a merge"));
}

#[test]
fn stack_step_of_unknown_size_is_uncertain() {
    let mut i = modify_i7(BASE, 0);
    i.imm = None;
    assert_eq!(
        problems(vec![i]),
        one("uncertain", "stack step of unknown size")
    );
}

#[test]
fn pc_relative_call_between_modifies_of_the_stack_pointer_is_balanced() {
    let mut call = cjump(BASE + 2);
    call.form = "25a_pcrel".to_string();
    let p = run(
        vec![
            insn(BASE, &[], &[]),
            modify_i7(BASE + 1, -1),
            call,
            push(BASE + 3, "R2"),
            push(BASE + 4, "R0"),
            modify_i7(BASE + 5, 1),
            insn(BASE + 6, &[], &[]),
        ],
        vec![
            Block {
                start: BASE,
                end: BASE + 5,
                term_pc: Some(BASE + 2),
                term: Terminator::Call {
                    conditional: false,
                    delayed: true,
                },
                delay_slots: vec![BASE + 3, BASE + 4],
                succ: vec![Edge {
                    to: BASE + 5,
                    kind: EdgeKind::ReturnSite,
                }],
            },
            block(
                BASE + 5,
                BASE + 7,
                Some(BASE + 6),
                Terminator::Return {
                    conditional: false,
                    delayed: false,
                },
                vec![],
            ),
        ],
    );
    assert_eq!(p, vec![]);
}

#[test]
fn restored_base_register_still_changes_the_index_register() {
    // B8 is saved, written and loaded back; the writes also load I8, which nothing saved.
    let p = problems(vec![
        push(BASE, "B8"),
        insn(BASE + 1, &["R0"], &["B8"]),
        pop(BASE + 2, "B8"),
    ]);
    assert_eq!(p, one("keep", "I8"));
}

#[test]
fn write_of_the_stack_base_register_is_a_stack_problem() {
    let p = problems(vec![insn(BASE, &["R0"], &["B7"])]);
    assert_eq!(
        p,
        vec![
            ("never_write", "B7".to_string()),
            ("stack", "I7".to_string())
        ]
    );
}

#[test]
fn b2w_of_the_stack_base_register_keeps_the_stack_pointer() {
    // Bd = B2W(Bs) (Type 7d) does not load I (PRM p.6-15). The DN2 interrupt stub converts I7, then B7, and keeps I7.
    let mut b2w = insn(BASE, &["B7"], &["B7"]);
    b2w.form = "7d".to_string();
    assert_eq!(problems(vec![b2w]), one("never_write", "B7"));
}

#[test]
fn w2b_of_a_base_register_does_not_write_the_index_register() {
    // Bd = W2B(Bs) (Type 7d) writes B8 only. An ordinary write of B8 also loads I8.
    let mut w2b = insn(BASE, &["B8"], &["B8"]);
    w2b.form = "7d".to_string();
    let i8_written = |p: Vec<(&'static str, String)>| p.iter().any(|(_, r)| r == "I8");
    assert!(!i8_written(problems(vec![w2b])));
    assert!(i8_written(problems(vec![insn(BASE, &["R0"], &["B8"])])));
}

#[test]
fn write_to_an_interrupt_register_is_never_allowed() {
    let p = problems(vec![insn(BASE, &["R0"], &["IMASK"])]);
    assert_eq!(p, one("never_write", "IMASK"));
}

#[test]
fn write_to_a_user_status_register_is_free() {
    assert_eq!(problems(vec![insn(BASE, &["R0"], &["USTAT1"])]), vec![]);
}

#[test]
fn write_to_a_register_in_no_class_is_unclassified() {
    let p = problems(vec![insn(BASE, &["R0"], &["FADDR"])]);
    assert_eq!(p, one("unclassified", "FADDR"));
}

#[test]
fn function_without_a_return_is_uncertain() {
    let p = run(
        vec![insn(BASE, &[], &[])],
        vec![block(BASE, BASE + 1, None, Terminator::Fall, vec![])],
    );
    assert_eq!(
        p,
        one("uncertain", "no return recognised; restore check not done")
    );
}

#[test]
fn frame_exit_without_a_frame_setup_is_a_problem() {
    let p = problems(vec![
        push(BASE, "I6"),
        insn(BASE + 1, &["I6"], &["I7"]),
        pop(BASE + 2, "I6"),
    ]);
    assert_eq!(p, one("stack", "I7"));
}
