//! Decoder checks on hand-assembled encodings (field layouts from the CFPRM
//! pages cited in tools/cfisa/coldfire.json). The whole-image comparison with
//! Ghidra, SLEIGH and Unicorn is tools/cfisa/oracle.py.

use coldfire::decode::FORM_COUNT;
use coldfire::{Ea, Form, Operand, Size, decode};

fn dec(pc: u32, w: &[u16]) -> Option<coldfire::Insn> {
    let mut words = [0u16; 3];
    words[..w.len()].copy_from_slice(w);
    decode(pc, words)
}

fn text(pc: u32, w: &[u16]) -> String {
    match dec(pc, w) {
        Some(i) => format!("{} [{}]", i, i.len),
        None => "illegal".to_string(),
    }
}

#[test]
fn integer_forms() {
    assert_eq!(text(0, &[0x7225]), "moveq.l #0x25,d1 [2]");
    assert_eq!(text(0, &[0x2f0a]), "move.l a2,-(a7) [2]");
    assert_eq!(text(0, &[0x4e75]), "rts [2]");
    assert_eq!(text(0, &[0x4eb9, 0x1234, 0x5678]), "jsr (0x12345678).l [6]");
    assert_eq!(text(0, &[0x46fc, 0x2700]), "move.w #0x2700,sr [4]");
    assert_eq!(text(0, &[0x2030, 0x0c00]), "move.l (0x0,a0,d0*0x4),d0 [4]");
    assert_eq!(text(0, &[0x4e7b, 0x0801]), "movec.l d0,vbr [4]");
    assert_eq!(text(0, &[0x508f]), "addq.l #0x8,a7 [2]");
    assert_eq!(text(0, &[0xa17a, 0x0010]), "illegal"); // MOV3Q to (d16,PC): not alterable
    assert_eq!(text(0, &[0xa140]), "mov3q.l #-0x1,d0 [2]");
    assert_eq!(text(0, &[0x71c1]), "mvz.w d1,d0 [2]");
}

#[test]
fn isa_c() {
    // RM p.97 Table 3-4, RM p.113
    assert_eq!(text(0, &[0x00c0]), "bitrev.l d0 [2]");
    assert_eq!(text(0, &[0x02c3]), "byterev.l d3 [2]");
    assert_eq!(text(0, &[0x04c7]), "ff1.l d7 [2]");
}

#[test]
fn divide_and_remainder_share_an_opword() {
    // CFPRM p.98 / p.135: Dw == Dq is DIVS.L, otherwise REMS.L
    let i = dec(0, &[0x4c45, 0x1801]).unwrap();
    assert_eq!(i.form, Form::DivsL);
    assert_eq!(i.to_string(), "divs.l d5,d1");
    let i = dec(0, &[0x4c45, 0x1802]).unwrap();
    assert_eq!(i.form, Form::RemsL);
    assert_eq!(i.to_string(), "rems.l d5,d2,d1");
}

#[test]
fn branches() {
    assert_eq!(text(0x1000, &[0x6000, 0x07de]), "bra.w 0x17e0 [4]");
    assert_eq!(text(0x1000, &[0x60ff, 0x0000, 0x0010]), "bra.l 0x1012 [6]");
    assert_eq!(text(0x1000, &[0x67fe]), "beq.b 0x1000 [2]");
    assert_eq!(text(0x1000, &[0x6100, 0xfffe]), "bsr.w 0x1000 [4]");
}

#[test]
fn tpf_lengths() {
    // CFPRM p.148: opmode 010 one word, 011 two words, 100 none
    assert_eq!(dec(0, &[0x51fa, 0x1234]).unwrap().len, 4);
    assert_eq!(dec(0, &[0x51fb, 0x1234, 0x5678]).unwrap().len, 6);
    assert_eq!(dec(0, &[0x51fc]).unwrap().len, 2);
    assert!(dec(0, &[0x51f9]).is_none());
}

#[test]
fn length_limit_and_ea_restrictions() {
    // 48-bit limit (CFPRM p.35): MOVE.L #imm,(d16,An) would be 8 bytes
    assert!(dec(0, &[0x217c, 0x1234, 0x5678]).is_none());
    // V4 allows MOVE.W #imm,(d16,An) (CFPRM p.113)
    assert_eq!(
        text(0, &[0x317c, 0x1234, 0x0010]),
        "move.w #0x1234,(0x10,a0) [6]"
    );
    // (d16,Ay) source to (xxx).L destination is excluded (CFPRM p.113)
    assert!(dec(0, &[0x23e8, 0x0004]).is_none());
    // TST.B An (CFPRM p.150), 68000-only byte arithmetic
    assert!(dec(0, &[0x4a08]).is_none());
    assert!(dec(0, &[0x9000]).is_none());
    assert_eq!(dec(0, &[0x4afc]).unwrap().form, Form::Illegal);
}

#[test]
fn emac() {
    // MAC with load: extension word first, then the (d16,Ay) displacement;
    // the ACC lsb in the opword is inverted (CFPRM p.172)
    let i = dec(0, &[0xa029, 0x0046, 0x0004]).unwrap();
    assert_eq!(i.form, Form::MacLoad);
    assert_eq!(i.len, 6);
    assert_eq!(i.size, Size::W);
    assert_eq!(i.to_string(), "mac.w d6.u,d0.l,(0x4,a1),d0,acc1");
    assert_eq!(i.operands()[3], Operand::Ea(Ea::Disp(1, 4)));
    assert_eq!(text(0, &[0xae01, 0x0800]), "mac.l d1,d7,acc0 [4]");
    assert_eq!(text(0, &[0xa1c0]), "movclr.l acc0,d0 [2]");
    assert_eq!(text(0, &[0xa380]), "move.l acc1,d0 [2]");
    assert_eq!(text(0, &[0xa93c, 0x0000, 0x0020]), "move.l #0x20,macsr [6]");
    assert_eq!(text(0, &[0xa9c0]), "move.l macsr,ccr [2]");
}

#[test]
fn pc_relative_base() {
    let i = dec(0x1000, &[0x41fa, 0xfffe]).unwrap();
    assert_eq!(
        i.operands()[0],
        Operand::Ea(Ea::PcDisp {
            base: 0x1002,
            d16: -2
        })
    );
}

#[test]
fn fpu_forms_decode() {
    // decoded for completeness; the MCF5441x has no FPU (line-F at run time)
    assert_eq!(text(0, &[0xf200, 0x0422]), "fadd.d fp1,fp0 [4]");
    assert_eq!(text(0, &[0xf210, 0x5422]), "fadd.d (a0),fp0 [4]");
}

#[test]
fn every_form_has_metadata() {
    assert_eq!(coldfire::decode::FORMS.len(), FORM_COUNT);
    for p in coldfire::decode::PAGES {
        assert!(p.contains("p."), "missing page citation: {}", p);
    }
}
