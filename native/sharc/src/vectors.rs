//! Block test vectors (tools/sharc_rsvec.py): a machine state at a block
//! entry, the memory the block reads, and what the Python reference
//! (tools/sharc_core) left at the block exit. Firmware-derived, under
//! out/. Plain text, one record per line:
//!
//! ```text
//! V <block> <sample>          start of a vector (hex block PC)
//! R <code> <value>            known register (decimal code, hex value)
//! X <code>                    unknown register
//! A <astatx> <mask> <astaty> <mask>
//! L <start> <end> <remaining> <mode>   loop stack entry, bottom first
//! C <pc> <pc> ...             PC stack, bottom first (may be empty)
//! M <addr> <hex bytes>        memory the block reads before writing
//! E                           the lines after this describe the exit
//! W <addr> <hex bytes>        final contents of every written byte
//! P <pc> <instructions> <pending>
//! END
//! ```

use crate::rt::{Int, Loop, NUREG, St, V};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub regs: Vec<(usize, u32)>,
    pub unknown: Vec<usize>,
    pub astat: [(u32, u32); 2],
    pub loops: Vec<Loop>,
    pub pcstk: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vector {
    pub block: u32,
    pub sample: u32,
    pub entry: Snapshot,
    pub mem_init: Vec<(u32, Vec<u8>)>,
    pub exit: Snapshot,
    pub writes: Vec<(u32, Vec<u8>)>,
    pub exit_pc: u32,
    pub instructions: u64,
    pub exit_pending: bool,
}

const UREG_ASTATX: usize = 118;
const UREG_ASTATY: usize = 119;

fn hex(s: &str) -> Result<u64, String> {
    u64::from_str_radix(s, 16).map_err(|e| format!("bad hex {s:?}: {e}"))
}

fn bytes(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd hex byte string {s:?}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

pub fn parse(text: &str) -> Result<Vec<Vector>, String> {
    let mut out = Vec::new();
    let mut cur: Option<Vector> = None;
    let mut in_exit = false;
    for (lineno, line) in text.lines().enumerate() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        let err = |m: String| format!("line {}: {m}", lineno + 1);
        if f[0] == "V" {
            cur = Some(Vector {
                block: hex(f[1]).map_err(err)? as u32,
                sample: f[2].parse().map_err(|e| err(format!("{e}")))?,
                ..Vector::default()
            });
            in_exit = false;
            continue;
        }
        let v = cur.as_mut().ok_or_else(|| err("record before V".into()))?;
        let snap = if in_exit { &mut v.exit } else { &mut v.entry };
        match f[0] {
            "R" => snap.regs.push((
                f[1].parse().map_err(|e| err(format!("{e}")))?,
                hex(f[2]).map_err(err)? as u32,
            )),
            "X" => snap
                .unknown
                .push(f[1].parse().map_err(|e| err(format!("{e}")))?),
            "A" => {
                let n: Vec<u32> = f[1..5]
                    .iter()
                    .map(|x| hex(x).map(|y| y as u32))
                    .collect::<Result<_, _>>()
                    .map_err(err)?;
                snap.astat = [(n[0], n[1]), (n[2], n[3])];
            }
            "L" => snap.loops.push(Loop {
                start_sw: hex(f[1]).map_err(err)? as i64,
                end_sw: hex(f[2]).map_err(err)? as i64,
                remaining: f[3].parse::<i64>().map_err(|e| err(format!("{e}")))?,
                mode: f[4].parse::<i64>().map_err(|e| err(format!("{e}")))?,
            }),
            "C" => {
                snap.pcstk = f[1..]
                    .iter()
                    .map(|x| hex(x).map(|y| y as u32))
                    .collect::<Result<_, _>>()
                    .map_err(err)?
            }
            "M" => v
                .mem_init
                .push((hex(f[1]).map_err(err)? as u32, bytes(f[2]).map_err(err)?)),
            "W" => v
                .writes
                .push((hex(f[1]).map_err(err)? as u32, bytes(f[2]).map_err(err)?)),
            "E" => in_exit = true,
            "P" => {
                v.exit_pc = hex(f[1]).map_err(err)? as u32;
                v.instructions = hex(f[2]).map_err(err)?;
                v.exit_pending = f[3] != "0";
            }
            "END" => out.push(cur.take().unwrap()),
            other => return Err(err(format!("unknown record {other:?}"))),
        }
    }
    if cur.is_some() {
        return Err("vector without END".into());
    }
    Ok(out)
}

fn astat_value(bits: u32, mask: u32) -> V {
    match mask {
        u32::MAX => V::c(bits as Int),
        0 => V::UNK,
        m => V::partial(m as Int, bits as Int),
    }
}

/// Load the vector's entry state and the memory it reads into `s`.
pub fn load_entry(s: &mut St, v: &Vector) {
    s.r = [V::UNK; NUREG];
    for &(code, value) in &v.entry.regs {
        s.r[code] = V::c(value as Int);
    }
    for &code in &v.entry.unknown {
        s.r[code] = V::UNK;
    }
    s.r[UREG_ASTATX] = astat_value(v.entry.astat[0].0, v.entry.astat[0].1);
    s.r[UREG_ASTATY] = astat_value(v.entry.astat[1].0, v.entry.astat[1].1);
    s.loops.clear();
    for l in &v.entry.loops {
        let _ = s.loops.push_raw(*l);
    }
    s.call_stack.clear();
    for &pc in &v.entry.pcstk {
        let _ = s.call_stack.push_raw(pc as Int);
    }
    s.status_stack.clear();
    s.pending = None;
    s.pc_sw = v.block as Int;
    for (addr, data) in &v.mem_init {
        for (k, &b) in data.iter().enumerate() {
            s.mem.write_byte(addr + k as u32, b);
        }
    }
    s.sync_snapshot();
    s.check_loops();
}

/// Compare `s` after running the vector's block with the reference exit;
/// one line per difference.
pub fn compare(s: &St, v: &Vector, exit_pc: u32) -> Vec<String> {
    let mut diffs = Vec::new();
    if exit_pc != v.exit_pc {
        diffs.push(format!("pc {exit_pc:#x} != {:#x}", v.exit_pc));
    }
    for &(code, value) in &v.exit.regs {
        if code == UREG_ASTATX || code == UREG_ASTATY {
            continue;
        }
        let got = s.r[code];
        if !got.is_c() {
            diffs.push(format!(
                "{} not known, reference {value:#010x}",
                ureg_name(code)
            ));
        } else if got.b != value {
            diffs.push(format!(
                "{} {:#010x} != reference {value:#010x}",
                ureg_name(code),
                got.b
            ));
        }
    }
    for &code in &v.exit.unknown {
        if code == UREG_ASTATX || code == UREG_ASTATY {
            continue;
        }
        if s.r[code].is_c() {
            diffs.push(format!(
                "{} known {:#010x}, reference Unknown",
                ureg_name(code),
                s.r[code].b
            ));
        }
    }
    for pe in 0..2 {
        let (bits, mask) = v.exit.astat[pe];
        let got = s.r[UREG_ASTATX + pe];
        if (got.b, got.m) != (bits & mask, mask) {
            diffs.push(format!(
                "ASTAT{} bits/mask {:#010x}/{:#010x} != reference {:#010x}/{:#010x}",
                ["X", "Y"][pe],
                got.b,
                got.m,
                bits & mask,
                mask
            ));
        }
    }
    if s.loops.items() != v.exit.loops.as_slice() {
        diffs.push(format!(
            "loops {:?} != reference {:?}",
            s.loops.items(),
            v.exit.loops
        ));
    }
    let pcstk: Vec<u32> = s.call_stack.items().iter().map(|&x| x as u32).collect();
    if pcstk != v.exit.pcstk {
        diffs.push(format!(
            "pc stack {pcstk:x?} != reference {:x?}",
            v.exit.pcstk
        ));
    }
    if s.pending.is_some() != v.exit_pending {
        diffs.push(format!(
            "pending {} != reference {}",
            s.pending.is_some(),
            v.exit_pending
        ));
    }
    for (addr, data) in &v.writes {
        // Read back the way the core does (the loader alias for a byte the
        // raw address does not hold), as the reference recorded it.
        let got: Vec<u8> = (0..data.len() as u32)
            .map(|k| {
                let a = addr + k;
                if !s.mem.present(a) && (a as i128) < crate::rt::bnd::SW_ALIAS_BASE {
                    s.mem
                        .byte(a.wrapping_add(crate::rt::bnd::SW_ALIAS_BASE as u32))
                } else {
                    s.mem.byte(a)
                }
            })
            .collect();
        if &got != data {
            diffs.push(format!(
                "memory {addr:#x}: {got:02x?} != reference {data:02x?}"
            ));
        }
    }
    diffs
}

pub fn ureg_name(code: usize) -> String {
    const SYS: [&str; 32] = [
        "FADDR",
        "DADDR",
        "UREG_RESERVED_62",
        "PC",
        "PCSTK",
        "PCSTKP",
        "LADDR",
        "CURLCNTR",
        "LCNTR",
        "EMUCLK",
        "EMUCLK2",
        "PX",
        "PX1",
        "PX2",
        "TPERIOD",
        "TCOUNT",
        "USTAT1",
        "USTAT2",
        "MODE1",
        "MMASK",
        "MODE2",
        "FLAGS",
        "ASTATX",
        "ASTATY",
        "STKYX",
        "STKYY",
        "IRPTL",
        "IMASK",
        "IMASKP",
        "MODE1STK",
        "USTAT3",
        "USTAT4",
    ];
    if code < 96 {
        format!("{}{}", ["R", "I", "M", "L", "B", "S"][code / 16], code % 16)
    } else if code < 128 {
        SYS[code - 96].to_string()
    } else {
        format!("ureg{code}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "V 1c4ecf 0\nR 0 5\nX 1\nA 1 ffffffff 0 0\nL 1c4ed0 1c4ed8 3 0\n\
C 1c4ed0\nM 1000 0a0b\nE\nR 0 6\nX 1\nA 0 1 0 0\nC\nW 1004 ff\nP 1c4ed9 a 0\nEND\n";

    #[test]
    fn parses_a_vector() {
        let vs = parse(SAMPLE).unwrap();
        assert_eq!(vs.len(), 1);
        let v = &vs[0];
        assert_eq!(v.block, 0x1c4ecf);
        assert_eq!(v.entry.regs, vec![(0, 5)]);
        assert_eq!(v.entry.loops[0].end_sw, 0x1c4ed8);
        assert_eq!(v.mem_init, vec![(0x1000, vec![0x0a, 0x0b])]);
        assert_eq!(v.writes, vec![(0x1004, vec![0xff])]);
        assert_eq!(
            (v.exit_pc, v.instructions, v.exit_pending),
            (0x1c4ed9, 10, false)
        );
    }

    #[test]
    fn loads_an_entry_state() {
        let v = &parse(SAMPLE).unwrap()[0];
        let mut s = St::new(crate::mem::Mem::new());
        load_entry(&mut s, v);
        assert_eq!(s.r[0], V::c(5));
        assert!(s.r[1].is_unknown());
        assert_eq!(s.r[UREG_ASTATX], V::c(1));
        assert_eq!(s.mem.byte(0x1001), 0x0b);
        assert_eq!(s.pc_sw, 0x1c4ecf);
    }
}
