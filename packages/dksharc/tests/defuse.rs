//! Register definitions and uses: the generated table against the core's semantics (golden vectors), the pluck machine's loop body
//! checked by hand against its assembler source, and synthetic text for the cases the pluck machine does not have.

use dksharc::defuse_table::{SPECIAL_NAMES, UREG_NAMES};
use dksharc::*;

const DKDS: &[u8] = include_bytes!("fixtures/pluck-dn2.dkds");
const DKO: &[u8] = include_bytes!("fixtures/pluck-dsp.dko.json");
const GOLDEN: &str = include_str!("fixtures/defuse-golden.json");

fn name(code: u64) -> String {
    let c = code as usize;
    if c < UREG_NAMES.len() {
        UREG_NAMES[c].to_string()
    } else {
        SPECIAL_NAMES[c - UREG_NAMES.len()].to_string()
    }
}

fn names(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| name(c.as_u64().unwrap()))
        .collect()
}

/// Every golden instruction: its form, its reads and writes, ASTATX bits, MODE1 effect, stack flag, condition and uncertainty are those
/// the generator computed from the core (and, for the writes of the forms the core can run, checked against it).
#[test]
fn golden_vectors_agree_with_the_generator() {
    let g: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() > 500, "{} cases", cases.len());
    let mut skipped = 0;
    for c in cases {
        let parcels: Vec<u16> = c["parcels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_u64().unwrap() as u16)
            .collect();
        // Followed by NOPs so that the width of the form is settled the way the decoder settles it in a program.
        let mut text = parcels.clone();
        text.extend(std::iter::repeat_n(0u16, 3 * 16));
        let a = analyze(&Input {
            text,
            base_sw: L2_SW_BASE,
            ..Input::default()
        });
        let form = c["form"].as_str().unwrap();
        let i = &a.insns[0];
        // The Python generator decodes without lookahead; where the two decoders pick another form of the same width class the case is
        // not about the table.
        if i.form != form || i.parcels as usize != parcels.len() {
            skipped += 1;
            continue;
        }
        // The next word decodes right behind it, at the width the table reports.
        assert_eq!(
            a.insns[1].pc,
            B + parcels.len() as u32,
            "next word of {form} {parcels:04x?}"
        );
        let d = &i.defuse;
        let ctx = format!("{form} {parcels:04x?}");
        assert_eq!(d.reads, names(&c["reads"]), "reads of {ctx}");
        assert_eq!(d.writes, names(&c["writes"]), "writes of {ctx}");
        assert_eq!(
            d.astat_bits as u64,
            c["astat"].as_u64().unwrap(),
            "astat of {ctx}"
        );
        assert_eq!(
            d.conditional,
            c["conditional"].as_bool().unwrap(),
            "cond of {ctx}"
        );
        assert_eq!(d.stack, c["stack"].as_bool().unwrap(), "stack of {ctx}");
        assert_eq!(
            !d.uncertain.is_empty(),
            !c["uncertain"].as_array().unwrap().is_empty(),
            "uncertain of {ctx}"
        );
        let m1 = match d.mode1.map(|m| m.op) {
            None => serde_json::Value::Null,
            Some(Mode1Op::Set) => "set".into(),
            Some(Mode1Op::Clear) => "clear".into(),
            Some(Mode1Op::Toggle) => "toggle".into(),
            Some(Mode1Op::PushSts) => "push_sts".into(),
            Some(Mode1Op::PopSts) => "pop_sts".into(),
            Some(Mode1Op::Full) => serde_json::Value::Null,
        };
        // A whole-register MODE1 write is derived from the written set; the generator reports only the bit operations and STS.
        if d.mode1.map(|m| m.op) != Some(Mode1Op::Full) {
            assert_eq!(m1, c["mode1"], "mode1 of {ctx}");
        }
    }
    eprintln!("golden: {} cases, {skipped} skipped", cases.len());
    assert!(
        skipped * 20 < cases.len(),
        "{skipped} of {} cases skipped",
        cases.len()
    );
}

fn pluck() -> Analysis {
    analyze(&Input::from_dkds(DKDS, DKO).unwrap())
}

/// The registers of an instruction without the flag and mode registers every compute touches.
fn data(d: &DefUse) -> (Vec<&str>, Vec<&str>) {
    fn keep(v: &[String]) -> Vec<&str> {
        v.iter()
            .map(String::as_str)
            .filter(|r| !matches!(*r, "ASTATX" | "STKYX" | "MODE1"))
            .collect()
    }
    (keep(&d.reads), keep(&d.writes))
}

/// The body of the 32 sample loop of `_pk_render`, line by line from the assembler source of the pluck machine.
#[test]
fn pluck_render_loop_body() {
    let a = pluck();
    let fi = a
        .functions
        .iter()
        .position(|f| f.name == "_pk_render")
        .unwrap();
    let l = a
        .loops
        .iter()
        .find(|l| l.function == Some(fi) && l.count == LoopCount::Lit { count: 32 })
        .unwrap();
    let body: Vec<_> = a
        .insns
        .iter()
        .filter(|i| i.pc >= l.start && i.pc <= l.end)
        .collect();
    // (reads, writes) in source order. A float operation is a compute on the same register numbers as the F name.
    let e: &[(&[&str], &[&str])] = &[
        (&["R3", "R14"], &["R1"]),                  // R1 = R3 + R14
        (&["R1"], &["I1"]),                         // I1 = R1
        (&["I1", "M5", "L1", "B1"], &["R0", "I1"]), // R0 = DM(I1, M5)
        (&["R4", "R14"], &["R14"]),                 // R14 = R4 + R14
        (&["R2", "R14"], &["R14"]),                 // R14 = R14 AND R2
        (&["R0", "R5"], &["R1"]),                   // F1 = F0 * F5
        (&["R6", "R10"], &["R8"]),                  // F8 = F10 * F6
        (&["R0"], &["R10"]),                        // F10 = PASS F0
        (&["R1", "R8"], &["R1"]),                   // F1 = F1 + F8
        (&["R1", "R12"], &["R8"]),                  // F8 = F1 - F12
        (&["R7", "R8"], &["R8"]),                   // F8 = F8 * F7
        (&["R8", "R11"], &["R8"]),                  // F8 = F8 + F11
        (&["R1"], &["R11"]),                        // F11 = PASS F1
        (&["R8"], &["R12"]),                        // F12 = PASS F8
        (&["R8", "R9"], &["R8"]),                   // F8 = F8 * F9
        (&["R3", "R15"], &["R1"]),                  // R1 = R3 + R15
        (&["R1"], &["I2"]),                         // I2 = R1
        (&["R8", "I2", "M5", "L2", "B2"], &["I2"]), // DM(I2, M5) = R8
        (&["R4", "R15"], &["R15"]),                 // R15 = R4 + R15
        (&["R2", "R15"], &["R15"]),                 // R15 = R15 AND R2
        (&["R8"], &["R0"]),                         // F0 = ABS F8
        (&["R0", "R13"], &["R13"]),                 // F13 = MAX(F13, F0)
        (&["R8", "I0", "M6", "L0", "B0"], &["I0"]), // DM(I0, M6) = R8 (the loop label)
    ];
    assert_eq!(body.len(), e.len());
    for (i, (r, w)) in body.iter().zip(e) {
        let (dr, dw) = data(&i.defuse);
        assert_eq!(
            (dr, dw),
            (r.to_vec(), w.to_vec()),
            "{} at +{:#x}",
            i.form,
            i.pc - a.base_sw
        );
        assert!(!i.defuse.conditional && i.defuse.uncertain.is_empty());
    }
    // The post-modify writes of the three memory accesses.
    let pm: Vec<_> = body
        .iter()
        .flat_map(|i| i.defuse.post_modify.iter())
        .collect();
    assert_eq!(pm, ["I1", "I2", "I0"]);
    // The function: no MODE1 write, the clobbered set follows the source, and the import call keeps it incomplete.
    let fx = &a.functions[fi].effects;
    assert!(fx.mode1.writes.is_empty());
    assert!(fx.returns && fx.other_exits.is_empty());
    assert_eq!(fx.unknown_calls, ["import:_dv_exp2"]);
    assert!(!fx.complete);
    for r in ["R0", "R15", "I0", "I1", "I2", "I3", "LCNTR"] {
        assert!(fx.writes.iter().any(|w| w == r), "{r}");
    }
    for r in ["I4", "I5", "M5", "M6", "L0", "B3"] {
        assert!(!fx.writes.iter().any(|w| w == r), "{r}");
    }
}

// ---- synthetic text -------------------------------------------------------------------------------------------------------------

const B: u32 = L2_SW_BASE;
const ALWAYS: u64 = 0x1f;

fn p48(f: u64) -> Vec<u16> {
    vec![(f >> 32) as u16, (f >> 16) as u16, f as u16]
}
fn rts() -> Vec<u16> {
    vec![0x0abe]
}
/// `IF cond Rn = Rx + Ry` (Type 2a, ALU opcode 0x01).
fn add(cond: u64, rn: u64, rx: u64, ry: u64) -> Vec<u16> {
    p48(0x0100_0000_0000 | cond << 33 | 0x01 << 12 | rn << 8 | rx << 4 | ry)
}
/// `Rn = DM(Ii, Mm)` (post-modify) or `Rn = DM(Mm, Ii)`, Type 3a with no compute; `store` writes Rn, `lw` is the long word access.
fn mem(post: bool, store: bool, lw: bool, pm: bool, i: u64, m: u64, ureg: u64) -> Vec<u16> {
    p48(0x4000_0000_0000
        | (post as u64) << 44
        | i << 41
        | m << 38
        | ALWAYS << 33
        | (pm as u64) << 32
        | (store as u64) << 31
        | (lw as u64) << 30
        | ureg << 23)
}
/// `BIT SET|CLR|TGL MODE1 mask` (Type 18a, system register 2 is MODE1).
fn mode1_bit(bop: u64, mask: u32) -> Vec<u16> {
    p48(0x1400_0000_0000 | bop << 37 | 2 << 32 | mask as u64)
}
/// `Rd = Rs` for Ureg codes (Type 5a, no compute).
fn mv(dst: u64, src: u64) -> Vec<u16> {
    p48(0x7000_0000_0000
        | (src >> 2) << 38
        | ALWAYS << 33
        | ((src >> 1) & 1) << 32
        | (src & 1) << 31
        | dst << 23)
}
/// `Ureg = data` (Type 17a).
fn imm(ureg: u64, data: u32) -> Vec<u16> {
    p48(0x0f00_0000_0000 | ureg << 32 | data as u64)
}
fn push_sts() -> Vec<u16> {
    p48(0x1700_0000_0000 | 1 << 37)
}
fn pop_sts() -> Vec<u16> {
    p48(0x1700_0000_0000 | 1 << 36)
}
fn jump_if(cond: u64, addr: u32) -> Vec<u16> {
    p48(0x0600_0000_0000 | cond << 33 | addr as u64)
}
fn call(addr: u32) -> Vec<u16> {
    p48(0x0600_0000_0000 | ALWAYS << 33 | 1 << 39 | addr as u64)
}
const MODE1: u64 = 114;

fn build(parts: &[Vec<u16>]) -> Analysis {
    let a = analyze(&Input {
        text: parts.concat(),
        base_sw: B,
        symbols: vec![TextSymbol {
            name: "_f".into(),
            parcel: 0,
            global: true,
        }],
        ..Input::default()
    });
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    assert!(a.undecoded.is_empty(), "{:?}", a.undecoded);
    a
}

fn d(a: &Analysis, k: usize) -> &DefUse {
    &a.insns[k].defuse
}

fn set(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn multifunction_compute() {
    // Float multiply with dual add and subtract (category 0x31): Rs = category & 15, Rm and Ra from the word; the inputs are
    // Fxm in F0-3, Fym in F4-7, Fxa in F8-11, Fya in F12-15 picked by two bit fields each.
    let w: u64 = 1 << 22 | 0x31 << 16 | 5 << 12 | 6 << 8 | 1 << 6 | 2 << 4 | 3 << 2 | 1;
    let a = build(&[p48(0x0100_0000_0000 | ALWAYS << 33 | w), rts()]);
    let x = d(&a, 0);
    assert_eq!(x.reads, set(&["R1", "R6", "R11", "R13"]));
    assert_eq!(x.writes, set(&["R1", "R5", "R6", "ASTATX", "STKYX"]));
    assert!(x.uncertain.is_empty() && !x.conditional);
    assert_eq!(x.simd_writes, set(&["S1", "S5", "S6", "ASTATY", "STKYY"]));
}

#[test]
fn dag_post_modify_and_pre_modify() {
    // R3 = DM(I2, M1): the post-modify reads I2, M1 and the circular state and writes I2.
    let a = build(&[
        mem(true, false, false, false, 2, 1, 3),
        mem(false, false, false, false, 2, 1, 3), // R3 = DM(M1, I2): pre-modify keeps I2
        mem(true, true, false, true, 1, 6, 4),    // PM(I9, M14) = R4 (DAG2)
        mem(true, false, true, false, 0, 0, 4),   // R4 = DM(I0, M0) (LW): the pair R4 and R5
        rts(),
    ]);
    assert_eq!(d(&a, 0).reads, set(&["I2", "M1", "L2", "B2"]));
    assert_eq!(d(&a, 0).writes, set(&["R3", "I2"]));
    assert_eq!(d(&a, 0).post_modify, set(&["I2"]));
    assert_eq!(d(&a, 1).reads, set(&["I2", "M1"]));
    assert_eq!(d(&a, 1).writes, set(&["R3"]));
    assert!(d(&a, 1).post_modify.is_empty());
    assert_eq!(d(&a, 2).reads, set(&["R4", "I9", "M14", "L9", "B9"]));
    assert_eq!(d(&a, 2).writes, set(&["I9"]));
    assert_eq!(d(&a, 3).writes, set(&["R4", "R5", "I0"]));
}

#[test]
fn width_ties_resolve_before_the_effects_are_read() {
    // A 16-bit short compute (`R1 = R1 + R2`, Type 2c) followed by a 48-bit compute: each reports its own registers, and the
    // second starts where the 16-bit one ends. Read as a wider form the 2c word would swallow the next one.
    let a = build(&[vec![0xc012], add(ALWAYS, 4, 5, 6), rts()]);
    let forms: Vec<_> = a
        .insns
        .iter()
        .map(|i| (i.pc - B, i.form.as_str()))
        .collect();
    assert_eq!(forms, [(0, "2c"), (1, "2a"), (4, "11c")], "{forms:?}");
    assert_eq!(d(&a, 0).reads, set(&["R1", "R2"]));
    assert_eq!(d(&a, 0).writes, set(&["R1", "ASTATX", "STKYX"]));
    assert_eq!(d(&a, 1).writes, set(&["R4", "ASTATX", "STKYX"]));
    assert_eq!(d(&a, 1).reads, set(&["R5", "R6", "MODE1"]));
}

#[test]
fn conditional_write() {
    let a = build(&[add(0, 1, 2, 3), add(ALWAYS, 1, 2, 3), rts()]);
    assert!(d(&a, 0).conditional);
    assert_eq!(d(&a, 0).writes, d(&a, 1).writes);
    assert!(!d(&a, 1).conditional);
}

#[test]
fn an_unmodelled_compute_is_uncertain_and_guesses_nothing() {
    // Compute unit 3 opcode 0x01: the core has no operation there.
    let a = build(&[
        p48(0x0100_0000_0000 | ALWAYS << 33 | 3 << 20 | 0x01 << 12 | 1 << 8 | 2 << 4 | 3),
        rts(),
    ]);
    let x = d(&a, 0);
    assert_eq!(x.uncertain.len(), 1);
    assert!(x.reads.is_empty() && x.writes.is_empty());
    assert!(!a.functions[0].effects.complete);
    assert_eq!(a.functions[0].effects.uncertain_pcs, [B]);
}

fn mode1(a: &Analysis) -> &Mode1Summary {
    &a.functions[0].effects.mode1
}

#[test]
fn mode1_bit_set_and_toggled_back() {
    // A bit toggled twice is back where it was; a bit set and then cleared is a known constant, not the entry value.
    let a = build(&[mode1_bit(2, 0x2000), mode1_bit(2, 0x2000), rts()]);
    assert_eq!(
        d(&a, 0).mode1,
        Some(Mode1Effect {
            op: Mode1Op::Toggle,
            mask: 0x2000,
            value: None
        })
    );
    assert_eq!(d(&a, 0).writes, set(&["MODE1"]));
    assert!(mode1(&a).restored && mode1(&a).changed_mask == 0 && mode1(&a).exact);
    assert_eq!(mode1(&a).writes.len(), 2);
    let b = build(&[mode1_bit(0, 0x2000), mode1_bit(1, 0x2000), rts()]);
    assert!(!mode1(&b).restored);
    assert_eq!(
        (mode1(&b).changed_mask, mode1(&b).unknown_mask),
        (0x2000, 0)
    );
}

#[test]
fn mode1_saved_in_a_register_and_restored() {
    // R0 = MODE1; BIT CLR MODE1 0x1000; ...; MODE1 = R0: the exact incoming value is back (sdk/dsp/sample/bank.inc LOCK and UNLOCK).
    let a = build(&[
        mv(0, MODE1),
        mode1_bit(1, 0x1000),
        add(ALWAYS, 1, 2, 3),
        mv(MODE1, 0),
        rts(),
    ]);
    assert_eq!(d(&a, 0).reads, set(&["MODE1"]));
    assert_eq!(d(&a, 3).mode1.map(|m| m.op), Some(Mode1Op::Full));
    assert!(mode1(&a).restored, "{:?}", mode1(&a));
    // The copy is overwritten before it is restored: the restore writes an unknown value.
    let b = build(&[
        mv(0, MODE1),
        mode1_bit(1, 0x1000),
        imm(0, 5),
        mv(MODE1, 0),
        rts(),
    ]);
    assert!(!mode1(&b).restored);
    assert_eq!(mode1(&b).changed_mask, u32::MAX);
    // A bit clear without the restore.
    let c = build(&[mv(0, MODE1), mode1_bit(1, 0x1000), rts()]);
    assert_eq!(
        (mode1(&c).changed_mask, mode1(&c).unknown_mask),
        (0x1000, 0)
    );
    // MODE1 = immediate is a known constant.
    let e = build(&[imm(MODE1, 0x0001_2000), rts()]);
    assert_eq!(
        d(&e, 0).mode1,
        Some(Mode1Effect {
            op: Mode1Op::Full,
            mask: u32::MAX,
            value: Some(0x12000)
        })
    );
    assert_eq!(mode1(&e).unknown_mask, 0);
}

#[test]
fn mode1_push_and_pop_status() {
    let a = build(&[push_sts(), mode1_bit(0, 0x2000), pop_sts(), rts()]);
    assert_eq!(d(&a, 0).mode1.map(|m| m.op), Some(Mode1Op::PushSts));
    assert!(d(&a, 0).stack);
    assert!(mode1(&a).restored, "{:?}", mode1(&a));
    // A push with no pop leaves MODE1 masked by MMASK.
    let b = build(&[push_sts(), rts()]);
    assert!(!mode1(&b).restored);
}

#[test]
fn mode1_changed_on_one_branch_only() {
    // 0: IF EQ JUMP +9 (over the set) | 3: BIT SET | 6: RTS ... the joined state at the return has the bit unknown.
    let a = build(&[jump_if(0, B + 9), mode1_bit(0, 0x4000), rts()]);
    assert!(!mode1(&a).restored);
    assert_eq!(mode1(&a).changed_mask, 0x4000);
    assert_eq!(mode1(&a).unknown_mask, 0x4000);
}

#[test]
fn writes_count_on_paths_to_the_return_only() {
    // 0 R1 = R2 + R3 | 3 IF EQ JUMP trap | 6 R4 = R2 + R3 | 9 RTS | 10 trap: R5 = R2 + R3 | 13 JUMP trap (never returns)
    let a = build(&[
        add(ALWAYS, 1, 2, 3),
        jump_if(0, B + 10),
        add(ALWAYS, 4, 2, 3),
        rts(),
        add(ALWAYS, 5, 2, 3),
        jump_if(ALWAYS, B + 10),
    ]);
    let fx = &a.functions[0].effects;
    assert!(fx.returns && fx.complete, "{fx:?}");
    assert_eq!(fx.writes, set(&["R1", "R4", "ASTATX", "STKYX"]));
}

#[test]
fn a_local_callee_adds_its_writes_and_an_import_makes_the_function_incomplete() {
    // 0 CALL g | 3 RTS | 4 g: R7 = R2 + R3 | 7 RTS
    let a = build(&[call(B + 4), rts(), add(ALWAYS, 7, 2, 3), rts()]);
    assert_eq!(a.functions.len(), 2);
    let f = &a.functions[0].effects;
    assert!(f.writes.contains(&"R7".to_string()), "{f:?}");
    assert!(f.complete);
    // The same call through an import relocation.
    let mut input = Input {
        text: [call(B + 4), rts()].concat(),
        base_sw: B,
        symbols: vec![TextSymbol {
            name: "_f".into(),
            parcel: 0,
            global: true,
        }],
        imports: vec!["_svc".into()],
        relocs: vec![TextReloc {
            parcel: 0,
            kind: 0,
            class: 2,
            import: 0,
        }],
        ..Input::default()
    };
    let b = analyze(&input);
    let f = &b.functions[0].effects;
    assert_eq!(f.unknown_calls, ["import:_svc"]);
    assert!(!f.complete && !f.mode1.exact);
    input.relocs.clear();
}

#[test]
fn the_do_loop_writes_its_counter_and_marks_the_stack() {
    let a = build(&[
        p48(0x0c00_0000_0000 | 4 << 24 | 6),
        add(ALWAYS, 1, 2, 3),
        add(ALWAYS, 4, 2, 3),
        rts(),
    ]);
    assert_eq!(d(&a, 0).writes, set(&["CURLCNTR", "LCNTR"]));
    assert!(d(&a, 0).stack);
}
