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
    }
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
fn frame_exit_without_a_frame_setup_is_a_problem() {
    let p = problems(vec![
        push(BASE, "I6"),
        insn(BASE + 1, &["I6"], &["I7"]),
        pop(BASE + 2, "I6"),
    ]);
    assert_eq!(p, one("stack", "I7"));
}
