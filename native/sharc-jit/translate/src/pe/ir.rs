//! The code the partial evaluator emits: structured WebAssembly with
//! symbolic labels and holes (filled once a join knows its locals), lowered
//! to `wasm_encoder` instructions at the end.

use wasm_encoder::{BlockType, Instruction, MemArg, ValType};

pub type Label = u32;
pub type Hole = u32;

#[derive(Clone, Debug)]
pub enum Node {
    I(Instruction<'static>),
    Block(Label, Vec<Node>),
    Loop(Label, Vec<Node>),
    /// Pops an i32 condition.
    If(Vec<Node>, Vec<Node>),
    Br(Label),
    BrIf(Label),
    Hole(Hole),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum W {
    I32,
    I64,
    F32,
    F64,
}

impl W {
    pub fn val(self) -> ValType {
        match self {
            W::I32 => ValType::I32,
            W::I64 => ValType::I64,
            W::F32 => ValType::F32,
            W::F64 => ValType::F64,
        }
    }
}

/// Locals of the function being built (after its parameters).
#[derive(Default)]
pub struct Locals {
    pub params: u32,
    pub types: Vec<W>,
}

impl Locals {
    pub fn new(params: u32) -> Locals {
        Locals {
            params,
            types: Vec::new(),
        }
    }
    pub fn add(&mut self, w: W) -> u32 {
        self.types.push(w);
        self.params + self.types.len() as u32 - 1
    }
}

pub fn mem(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

/// Code with no effect other than defining locals (pure): may be dropped.
pub fn is_pure(nodes: &[Node]) -> bool {
    nodes.iter().all(|n| match n {
        Node::I(i) => pure_insn(i),
        Node::Hole(_) => false,
        _ => false,
    })
}

fn pure_insn(i: &Instruction) -> bool {
    use Instruction::*;
    !matches!(
        i,
        Call(_)
            | CallIndirect { .. }
            | I32Store(_)
            | I64Store(_)
            | F32Store(_)
            | F64Store(_)
            | I32Store8(_)
            | I32Store16(_)
            | I64Store8(_)
            | I64Store16(_)
            | I64Store32(_)
            | Br(_)
            | BrIf(_)
            | BrTable(..)
            | Return
            | Unreachable
            | I32DivS
            | I32DivU
            | I64DivS
            | I64DivU
            | I32RemS
            | I32RemU
            | I64RemS
            | I64RemU
            | I32TruncF32S
            | I32TruncF64S
            | I64TruncF64S
            | I64TruncF32S
            | I32TruncF32U
            | I32TruncF64U
            | I64TruncF64U
            | I64TruncF32U
    )
}

/// Lower NODES into OUT; HOLES gives each hole's code.
pub fn lower(nodes: &[Node], holes: &[Vec<Node>], stack: &mut Vec<Label>, out: &mut Vec<Instruction<'static>>) {
    for n in nodes {
        match n {
            Node::I(i) => out.push(i.clone()),
            Node::Block(l, body) => {
                out.push(Instruction::Block(BlockType::Empty));
                stack.push(*l);
                lower(body, holes, stack, out);
                stack.pop();
                out.push(Instruction::End);
            }
            Node::Loop(l, body) => {
                out.push(Instruction::Loop(BlockType::Empty));
                stack.push(*l);
                lower(body, holes, stack, out);
                stack.pop();
                out.push(Instruction::End);
            }
            Node::If(a, b) => {
                out.push(Instruction::If(BlockType::Empty));
                stack.push(u32::MAX);
                lower(a, holes, stack, out);
                if !b.is_empty() {
                    out.push(Instruction::Else);
                    lower(b, holes, stack, out);
                }
                stack.pop();
                out.push(Instruction::End);
            }
            Node::Br(l) => out.push(Instruction::Br(depth(stack, *l))),
            Node::BrIf(l) => out.push(Instruction::BrIf(depth(stack, *l))),
            Node::Hole(h) => lower(&holes[*h as usize], holes, stack, out),
        }
    }
}

fn depth(stack: &[Label], l: Label) -> u32 {
    for (k, &x) in stack.iter().rev().enumerate() {
        if x == l {
            return k as u32;
        }
    }
    panic!("label {l} not in scope");
}
