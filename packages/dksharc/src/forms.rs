//! What a decoded form means for control flow and memory. The decoder gives raw fields; their meaning comes from the form name
//! (SHARC+ Core Programming Reference, instruction types 1 to 25). Nothing here guesses: an unknown form is a plain instruction
//! with no memory operand, and the forms that transfer control without a model here are returned as [`Flow::Unmodelled`].

use crate::model::{Access, Direction, MemOp, Mode, Space};
use sharc_rt::decode::Decoded;

/// The fields of one decoded instruction.
pub struct Fields<'a>(&'a Decoded);

impl<'a> Fields<'a> {
    pub fn new(d: &'a Decoded) -> Self {
        Fields(d)
    }
    pub fn form(&self) -> &'static str {
        self.0.type_name
    }
    /// The field `stem`: one value, or the pieces `stem[hi:lo]` joined into a number.
    pub fn get(&self, stem: &str) -> Option<i64> {
        let mut found = false;
        let mut v = 0i64;
        for f in self.0.fields() {
            if f.key == stem {
                return Some(f.value);
            }
            let Some(rest) = f.key.strip_prefix(stem).and_then(|r| r.strip_prefix('[')) else {
                continue;
            };
            let lo: u32 = rest
                .trim_end_matches(']')
                .split(':')
                .next_back()?
                .parse()
                .ok()?;
            found = true;
            v |= f.value << lo;
        }
        found.then_some(v)
    }
    fn bit(&self, stem: &str) -> bool {
        self.get(stem) == Some(1)
    }
}

fn signed(v: i64, bits: u32) -> i64 {
    let m = 1i64 << (bits - 1);
    (v & (m - 1)) - (v & m)
}

/// The condition code that means "always".
pub const COND_ALWAYS: i64 = 0x1f;

/// The control transfer of an instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    Jump {
        target: Option<u32>,
        conditional: bool,
        delayed: bool,
    },
    /// An indirect jump `JUMP (Mm, Ii)`.
    JumpIndirect {
        i: u8,
        m: u8,
        conditional: bool,
        delayed: bool,
    },
    Call {
        target: Option<u32>,
        conditional: bool,
        delayed: bool,
    },
    CallIndirect {
        i: u8,
        m: u8,
        conditional: bool,
        delayed: bool,
    },
    Return {
        conditional: bool,
        delayed: bool,
    },
    Unmodelled,
}

impl Flow {
    pub fn delayed(&self) -> bool {
        match self {
            Flow::Jump { delayed, .. }
            | Flow::JumpIndirect { delayed, .. }
            | Flow::Call { delayed, .. }
            | Flow::CallIndirect { delayed, .. }
            | Flow::Return { delayed, .. } => *delayed,
            Flow::Unmodelled => false,
        }
    }
}

pub fn flow(d: &Decoded, pc: u32) -> Option<Flow> {
    let f = Fields(d);
    let cond = |f: &Fields| f.get("cond").is_some_and(|c| c != COND_ALWAYS);
    let rel = |off: i64| ((pc as i64 + off) & 0xff_ffff) as u32;
    Some(match f.form() {
        "8a_abs" | "8a_rel" | "9a_rel" | "9b_rel" => {
            let target = match f.form() {
                "8a_abs" => f.get("addr").map(|a| a as u32),
                "8a_rel" => f.get("reladdr").map(|r| rel(signed(r, 24))),
                _ => f.get("reladdr").map(|r| rel(signed(r, 6))),
            };
            let (conditional, delayed) = (cond(&f), f.bit("j"));
            if f.bit("b") {
                Flow::Call {
                    target,
                    conditional,
                    delayed,
                }
            } else {
                Flow::Jump {
                    target,
                    conditional,
                    delayed,
                }
            }
        }
        "9a_abs" | "9b_abs" => {
            let i = f.get("pmi")? as u8 + 8;
            let m = f.get("pmm")? as u8 + 8;
            let (conditional, delayed) = (cond(&f), f.bit("j"));
            if f.bit("b") {
                Flow::CallIndirect {
                    i,
                    m,
                    conditional,
                    delayed,
                }
            } else {
                Flow::JumpIndirect {
                    i,
                    m,
                    conditional,
                    delayed,
                }
            }
        }
        // CJUMP (the compiler's call: it also sets up the frame) always has two delay slots.
        "25a_direct" => Flow::Call {
            target: f.get("addr").map(|a| a as u32),
            conditional: false,
            delayed: true,
        },
        "25a_pcrel" => Flow::Call {
            target: f.get("reladdr").map(|r| rel(signed(r, 24))),
            conditional: false,
            delayed: true,
        },
        "11a" | "11c" => {
            if f.bit("x") || f.bit("lr") {
                // RTI or a loop re-entry: not a plain return.
                Flow::Unmodelled
            } else {
                Flow::Return {
                    conditional: cond(&f),
                    delayed: f.bit("j"),
                }
            }
        }
        // JUMP with a data move (Type 10), and the undocumented transfer forms.
        "10a_rel" | "8p_undoc48" => Flow::Unmodelled,
        _ => return None,
    })
}

/// A `DO` loop: its count and the offset of its last instruction from the DO.
pub enum DoCount {
    Lit(u32),
    Reg(u8),
    Until(u8),
}

pub fn do_loop(d: &Decoded) -> Option<(DoCount, i64)> {
    let f = Fields(d);
    let count = match f.form() {
        "12a_imm" => DoCount::Lit(f.get("data")? as u32),
        "12a_ureg" => DoCount::Reg(f.get("ureg")? as u8),
        "13a" => DoCount::Until(f.get("term")? as u8),
        _ => return None,
    };
    Some((count, signed(f.get("reladdr")?, 23)))
}

/// Name of a Ureg code (SHARC+ Core Programming Reference, "Universal registers"): data, DAG and a few system registers.
pub fn ureg_name(code: u8) -> String {
    match code {
        0x00..=0x0f => format!("R{code}"),
        0x10..=0x4f => format!(
            "{}{}",
            ["I", "M", "L", "B"][(code >> 4) as usize - 1],
            code & 0xf
        ),
        _ => format!("ureg{code:#04x}"),
    }
}

fn mem(
    space: Space,
    store: bool,
    mode: Mode,
    access: Access,
    i: Option<i64>,
    m: Option<i64>,
) -> MemOp {
    MemOp {
        space,
        dir: if store {
            Direction::Store
        } else {
            Direction::Load
        },
        mode,
        access,
        i: i.map(|v| v as u8),
        m: m.map(|v| v as u8),
        imm: None,
        addr: None,
        import: None,
    }
}

/// The memory accesses of the instruction, from its form.
pub fn mem_ops(d: &Decoded) -> Vec<MemOp> {
    let f = Fields(d);
    let bank = |f: &Fields| if f.bit("g") { 8 } else { 0 };
    let space = |f: &Fields| if f.bit("g") { Space::Pm } else { Space::Dm };
    let width = |f: &Fields| {
        if f.bit("l") {
            Access::Long
        } else {
            Access::Normal
        }
    };
    let post = |f: &Fields| {
        if f.bit("u") {
            Mode::PostModify
        } else {
            Mode::PreModify
        }
    };
    let g = |v: Option<i64>, b: i64| v.map(|v| v + b);
    match f.form() {
        // Two accesses in parallel with a compute: DM through I0-I7 / M0-M7, PM through I8-I15 / M8-M15, both post-modify.
        "1a" | "1b" => {
            let pmi = f.get("pmi");
            let pmm = f.get("pmm");
            vec![
                mem(
                    Space::Dm,
                    f.bit("dmd"),
                    Mode::PostModify,
                    Access::Normal,
                    f.get("dmi"),
                    f.get("dmm"),
                ),
                mem(
                    Space::Pm,
                    f.bit("pmd"),
                    Mode::PostModify,
                    Access::Normal,
                    g(pmi, 8),
                    g(pmm, 8),
                ),
            ]
        }
        "3a" => vec![mem(
            space(&f),
            f.bit("d"),
            post(&f),
            width(&f),
            g(f.get("i"), bank(&f)),
            g(f.get("m"), bank(&f)),
        )],
        "3b" | "3d" => vec![mem(
            space(&f),
            f.bit("d"),
            post(&f),
            Access::SubWord,
            g(f.get("i"), bank(&f)),
            g(f.get("m"), bank(&f)),
        )],
        "3c" => vec![mem(
            Space::Dm,
            f.bit("d"),
            Mode::PostModify,
            Access::Normal,
            f.get("dmi"),
            f.get("dmm"),
        )],
        "6a_mem" => vec![mem(
            space(&f),
            f.bit("d"),
            Mode::PostModify,
            Access::Normal,
            g(f.get("i"), bank(&f)),
            g(f.get("m"), bank(&f)),
        )],
        "4a" | "4b" | "4d" => {
            let mut op = mem(
                space(&f),
                f.bit("d"),
                if f.bit("u") {
                    Mode::PostModifyImm
                } else {
                    Mode::PreModifyImm
                },
                if f.form() == "4a" {
                    Access::Normal
                } else {
                    Access::SubWord
                },
                g(f.get("i"), bank(&f)),
                None,
            );
            op.imm = f.get("data").map(|v| signed(v, 6));
            vec![op]
        }
        "10a_rel" => vec![mem(
            Space::Dm,
            f.bit("d"),
            Mode::PostModify,
            Access::Normal,
            f.get("dmi"),
            f.get("dmm"),
        )],
        "14a" => {
            let mut op = mem(space(&f), f.bit("d"), Mode::Direct, width(&f), None, None);
            op.addr = f.get("addr").map(|a| a as u32);
            vec![op]
        }
        "14d" => {
            let mut op = mem(
                Space::Dm,
                f.bit("d"),
                Mode::Direct,
                Access::SubWord,
                None,
                None,
            );
            op.addr = f.get("addr").map(|a| a as u32);
            vec![op]
        }
        "15a" => {
            let mut op = mem(
                space(&f),
                f.bit("d"),
                Mode::PreModifyImm,
                width(&f),
                g(f.get("i"), bank(&f)),
                None,
            );
            op.imm = f.get("addr").map(|v| signed(v, 32));
            vec![op]
        }
        "15b" => {
            let mut op = mem(
                space(&f),
                f.bit("d"),
                Mode::PreModifyImm,
                width(&f),
                g(f.get("i"), bank(&f)),
                None,
            );
            op.imm = f.get("data").map(|v| signed(v, 7));
            vec![op]
        }
        _ => vec![],
    }
}
