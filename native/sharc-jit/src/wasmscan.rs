//! A small WebAssembly (MVP + sign-ext, saturating conversions, bulk
//! memory, multi-value) reader and function-body rewriter, enough to lift
//! functions out of a module built by rustc/wasm-ld into modules of their
//! own. It is measurement tooling for the P1 gate (`jit-probe`): the block
//! functions of the ahead-of-time build stand in for what a run-time
//! translator would emit.

use std::collections::HashMap;
use std::ops::Range;

pub fn uleb(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let x = b[*p];
        *p += 1;
        v |= ((x & 0x7f) as u64) << shift;
        if x & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

fn skip_leb(b: &[u8], p: &mut usize) {
    while b[*p] & 0x80 != 0 {
        *p += 1;
    }
    *p += 1;
}

fn sleb(b: &[u8], p: &mut usize) -> i64 {
    let (mut v, mut shift) = (0i64, 0);
    loop {
        let x = b[*p];
        *p += 1;
        v |= ((x & 0x7f) as i64) << shift;
        shift += 7;
        if x & 0x80 == 0 {
            if shift < 64 && x & 0x40 != 0 {
                v |= -1i64 << shift;
            }
            return v;
        }
    }
}

pub fn put_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let x = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(x);
            return;
        }
        out.push(x | 0x80);
    }
}

fn put_sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let x = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && x & 0x40 == 0) || (v == -1 && x & 0x40 != 0);
        out.push(if done { x } else { x | 0x80 });
        if done {
            return;
        }
    }
}

fn name(b: &[u8], p: &mut usize) -> String {
    let n = uleb(b, p) as usize;
    let s = String::from_utf8_lossy(&b[*p..*p + n]).into_owned();
    *p += n;
    s
}

/// A function type: parameter and result value-type bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FuncType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

pub struct Module<'a> {
    pub bytes: &'a [u8],
    /// (section id, payload range), in order.
    pub sections: Vec<(u8, Range<usize>)>,
    pub types: Vec<FuncType>,
    /// Type of every function (imports first).
    pub func_types: Vec<u32>,
    pub n_imported_funcs: u32,
    /// (value type, mutable) of every global (imports first).
    pub globals: Vec<(u8, bool)>,
    /// Body range of each defined function (index - n_imported_funcs).
    pub bodies: Vec<Range<usize>>,
    pub names: HashMap<u32, String>,
    /// Table slot of each function placed by an active element segment.
    pub table_slot: HashMap<u32, u32>,
    pub exports: Vec<(String, u8, u32)>,
}

fn skip_limits(b: &[u8], p: &mut usize) {
    let flags = b[*p];
    *p += 1;
    skip_leb(b, p);
    if flags & 1 != 0 {
        skip_leb(b, p);
    }
}

fn const_expr_i32(b: &[u8], p: &mut usize) -> i64 {
    let op = b[*p];
    *p += 1;
    let v = match op {
        0x41 | 0x42 => sleb(b, p),
        0x23 => {
            skip_leb(b, p);
            -1
        }
        _ => panic!("constant expression opcode {op:#x}"),
    };
    assert_eq!(b[*p], 0x0b, "constant expression end");
    *p += 1;
    v
}

impl<'a> Module<'a> {
    pub fn parse(bytes: &'a [u8]) -> Module<'a> {
        assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0", "not a wasm module");
        let mut m = Module {
            bytes,
            sections: Vec::new(),
            types: Vec::new(),
            func_types: Vec::new(),
            n_imported_funcs: 0,
            globals: Vec::new(),
            bodies: Vec::new(),
            names: HashMap::new(),
            table_slot: HashMap::new(),
            exports: Vec::new(),
        };
        let b = bytes;
        let mut p = 8;
        while p < b.len() {
            let id = b[p];
            p += 1;
            let n = uleb(b, &mut p) as usize;
            let r = p..p + n;
            m.sections.push((id, r.clone()));
            let mut q = r.start;
            match id {
                1 => {
                    for _ in 0..uleb(b, &mut q) {
                        assert_eq!(b[q], 0x60, "function type");
                        q += 1;
                        let np = uleb(b, &mut q) as usize;
                        let params = b[q..q + np].to_vec();
                        q += np;
                        let nr = uleb(b, &mut q) as usize;
                        let results = b[q..q + nr].to_vec();
                        q += nr;
                        m.types.push(FuncType { params, results });
                    }
                }
                2 => {
                    for _ in 0..uleb(b, &mut q) {
                        name(b, &mut q);
                        name(b, &mut q);
                        let kind = b[q];
                        q += 1;
                        match kind {
                            0 => {
                                m.func_types.push(uleb(b, &mut q) as u32);
                                m.n_imported_funcs += 1;
                            }
                            1 => {
                                q += 1;
                                skip_limits(b, &mut q);
                            }
                            2 => skip_limits(b, &mut q),
                            3 => {
                                m.globals.push((b[q], b[q + 1] != 0));
                                q += 2;
                            }
                            _ => panic!("import kind {kind}"),
                        }
                    }
                }
                3 => {
                    for _ in 0..uleb(b, &mut q) {
                        m.func_types.push(uleb(b, &mut q) as u32);
                    }
                }
                6 => {
                    for _ in 0..uleb(b, &mut q) {
                        m.globals.push((b[q], b[q + 1] != 0));
                        q += 2;
                        const_expr_i32(b, &mut q);
                    }
                }
                7 => {
                    for _ in 0..uleb(b, &mut q) {
                        let nm = name(b, &mut q);
                        let kind = b[q];
                        q += 1;
                        let idx = uleb(b, &mut q) as u32;
                        m.exports.push((nm, kind, idx));
                    }
                }
                9 => {
                    for _ in 0..uleb(b, &mut q) {
                        let flags = uleb(b, &mut q);
                        assert_eq!(flags, 0, "element segment flags");
                        let off = const_expr_i32(b, &mut q) as u32;
                        for k in 0..uleb(b, &mut q) as u32 {
                            let f = uleb(b, &mut q) as u32;
                            m.table_slot.insert(f, off + k);
                        }
                    }
                }
                10 => {
                    for _ in 0..uleb(b, &mut q) {
                        let n = uleb(b, &mut q) as usize;
                        m.bodies.push(q..q + n);
                        q += n;
                    }
                }
                0 if name(b, &mut q) == "name" => {
                    while q < r.end {
                        let sub = b[q];
                        q += 1;
                        let n = uleb(b, &mut q) as usize;
                        let end = q + n;
                        if sub == 1 {
                            for _ in 0..uleb(b, &mut q) {
                                let i = uleb(b, &mut q) as u32;
                                m.names.insert(i, name(b, &mut q));
                            }
                        }
                        q = end;
                    }
                }
                _ => {}
            }
            p = r.end;
        }
        m
    }

    pub fn body(&self, func: u32) -> &'a [u8] {
        &self.bytes[self.bodies[(func - self.n_imported_funcs) as usize].clone()]
    }
}

/// An index a function body refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ref {
    Func(u32),
    Global(u32),
    Type(u32),
    Table(u32),
}

/// Copy a function body (locals and code), passing every function, global,
/// type and table index through MAP. With an identity MAP it is a scan.
pub fn rewrite(body: &[u8], map: &mut dyn FnMut(Ref) -> u32) -> Vec<u8> {
    let b = body;
    let mut out = Vec::with_capacity(b.len());
    let mut p = 0;
    // locals
    let n = uleb(b, &mut p);
    for _ in 0..n {
        skip_leb(b, &mut p);
        p += 1;
    }
    out.extend_from_slice(&b[..p]);
    let copy_leb = |out: &mut Vec<u8>, p: &mut usize| {
        let s = *p;
        skip_leb(b, p);
        out.extend_from_slice(&b[s..*p]);
    };
    while p < b.len() {
        let op = b[p];
        out.push(op);
        p += 1;
        match op {
            0x02..=0x04 => {
                let t = b[p];
                if t == 0x40 || (0x6f..=0x7f).contains(&t) {
                    out.push(t);
                    p += 1;
                } else {
                    let i = sleb(b, &mut p);
                    put_sleb(&mut out, map(Ref::Type(i as u32)) as i64);
                }
            }
            0x0c | 0x0d | 0x20..=0x22 => copy_leb(&mut out, &mut p),
            0x0e => {
                let n = uleb(b, &mut p);
                put_uleb(&mut out, n);
                for _ in 0..=n {
                    copy_leb(&mut out, &mut p);
                }
            }
            0x10 | 0x12 | 0xd2 => {
                let f = uleb(b, &mut p) as u32;
                put_uleb(&mut out, map(Ref::Func(f)) as u64);
            }
            0x11 | 0x13 => {
                let t = uleb(b, &mut p) as u32;
                put_uleb(&mut out, map(Ref::Type(t)) as u64);
                let tb = uleb(b, &mut p) as u32;
                put_uleb(&mut out, map(Ref::Table(tb)) as u64);
            }
            0x1c => {
                let n = uleb(b, &mut p) as usize;
                put_uleb(&mut out, n as u64);
                out.extend_from_slice(&b[p..p + n]);
                p += n;
            }
            0x23 | 0x24 => {
                let g = uleb(b, &mut p) as u32;
                put_uleb(&mut out, map(Ref::Global(g)) as u64);
            }
            0x25 | 0x26 => {
                let t = uleb(b, &mut p) as u32;
                put_uleb(&mut out, map(Ref::Table(t)) as u64);
            }
            0x28..=0x3e => {
                let a = uleb(b, &mut p);
                put_uleb(&mut out, a);
                if a & 0x40 != 0 {
                    copy_leb(&mut out, &mut p);
                }
                copy_leb(&mut out, &mut p);
            }
            0x3f | 0x40 => copy_leb(&mut out, &mut p),
            0x41 | 0x42 => copy_leb(&mut out, &mut p),
            0x43 => {
                out.extend_from_slice(&b[p..p + 4]);
                p += 4;
            }
            0x44 => {
                out.extend_from_slice(&b[p..p + 8]);
                p += 8;
            }
            0xd0 => {
                out.push(b[p]);
                p += 1;
            }
            0xfc => {
                let sub = uleb(b, &mut p);
                put_uleb(&mut out, sub);
                match sub {
                    0..=7 => {}
                    10 | 14 => {
                        copy_leb(&mut out, &mut p);
                        copy_leb(&mut out, &mut p);
                    }
                    11 => copy_leb(&mut out, &mut p),
                    15..=17 => {
                        let t = uleb(b, &mut p) as u32;
                        put_uleb(&mut out, map(Ref::Table(t)) as u64);
                    }
                    _ => panic!("0xfc {sub} (segment operations) in a lifted function"),
                }
            }
            0xfd => panic!("SIMD in a lifted function"),
            0x00 | 0x01 | 0x05 | 0x0b | 0x0f | 0x1a | 0x1b | 0xd1 | 0x45..=0xc4 => {}
            _ => panic!("opcode {op:#x}"),
        }
    }
    out
}

/// Every index BODY refers to.
pub fn refs(body: &[u8]) -> Vec<Ref> {
    let mut v = Vec::new();
    rewrite(body, &mut |r| {
        v.push(r);
        match r {
            Ref::Func(x) | Ref::Global(x) | Ref::Type(x) | Ref::Table(x) => x,
        }
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb_round_trips() {
        for v in [0u64, 1, 127, 128, 300, 1 << 20, u32::MAX as u64] {
            let mut o = Vec::new();
            put_uleb(&mut o, v);
            let mut p = 0;
            assert_eq!(uleb(&o, &mut p), v);
            assert_eq!(p, o.len());
        }
        for v in [0i64, -1, 63, 64, -64, -65, i32::MIN as i64, i64::MAX] {
            let mut o = Vec::new();
            put_sleb(&mut o, v);
            let mut p = 0;
            assert_eq!(sleb(&o, &mut p), v);
        }
    }

    #[test]
    fn rewrite_remaps_calls_and_globals_only() {
        // no locals; global.get 0; call 5; i32.const -1; drop; drop; end
        let body = [0x00, 0x23, 0x00, 0x10, 0x05, 0x41, 0x7f, 0x1a, 0x1a, 0x0b];
        let out = rewrite(&body, &mut |r| match r {
            Ref::Func(5) => 200,
            Ref::Global(0) => 3,
            Ref::Func(x) | Ref::Global(x) | Ref::Type(x) | Ref::Table(x) => x,
        });
        assert_eq!(
            out,
            [
                0x00, 0x23, 0x03, 0x10, 0xc8, 0x01, 0x41, 0x7f, 0x1a, 0x1a, 0x0b
            ]
        );
        assert_eq!(refs(&body), [Ref::Global(0), Ref::Func(5)]);
    }
}
