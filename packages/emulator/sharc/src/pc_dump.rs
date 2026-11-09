//! Interpreter register dump (diagnostic). `SHARC_PCDUMP=pc[,pc...]|all` (hex
//! software PCs), `SHARC_PCDUMP_OUT=path.csv`, optional
//! `SHARC_PCDUMP_ICOUNT=lo:hi` (decimal engine icount window, inclusive; default
//! all) and `SHARC_PCDUMP_LIMIT=n` (max rows, default 10000).
//!
//! Just before an interpreter single step executes an instruction whose PC is
//! in the set (and whose icount is in the window) one row is appended:
//! `icount,pc`, R0..R15 (hex), the same 16 values as f32 (`{:e}`), I0..I15,
//! M0..M15, ASTATX, ASTATY, STKYX, MODE1, MODE2, S0..S15 (hex), then the depth of the PC stack and of the
//! loop stack. A last `#stack-max` line gives the deepest PC, loop, status and call stacks of any interpreter step and
//! the pc of the first step that reached each (`SHARC_PCDUMP=0` records only that).
//!
//! The engine keeps each register as a 32-bit value, not the 40-bit hardware
//! extended word, so the hex is all 32 bits held and the f32 view is those same
//! 32 bits reinterpreted (the model has no low 8 bits to drop). A register whose
//! value is not fully known (partial mask) is shown with its known bits only.
//!
//! Only interpreter steps are hooked. For every instruction to be one run with
//! `SHARC_AOT=0 SHARC_FAST_REGIONS= SHARC_BLOCKTRANS=`.

use std::io::Write;

pub struct PcDump {
    /// `SHARC_PCDUMP_MEM=addr:words`: that many 32-bit words from the byte
    /// address (hex) are appended to every row.
    mem: Option<(u32, u32)>,
    out: std::io::BufWriter<std::fs::File>,
    pcs: Vec<u64>,
    /// Deepest PC, loop, status and call stacks seen at any interpreter step, and the pc where each was first reached
    /// (written as a `#stack-max` line when the dump closes).
    deepest: [(usize, u64); 4],
    lo: u64,
    hi: u64,
    left: u64,
}

impl PcDump {
    pub fn from_env() -> Option<Box<PcDump>> {
        let spec = std::env::var("SHARC_PCDUMP").ok()?;
        let path = crate::diag::out_path("SHARC_PCDUMP_OUT", "pcdump.csv")?;
        // `{pid}` in the path becomes the process id (several emulators of one run)
        let path = std::ffi::OsString::from(
            path.to_string_lossy()
                .replace("{pid}", &std::process::id().to_string()),
        );
        let mut pcs = Vec::new();
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if item == "all" {
                pcs.push(u64::MAX);
                continue;
            }
            let Ok(a) = u64::from_str_radix(item.trim_start_matches("0x"), 16) else {
                eprintln!("SHARC_PCDUMP: bad item {item:?}; pc dump disabled");
                return None;
            };
            pcs.push(a);
        }
        if pcs.is_empty() {
            return None;
        }
        let (mut lo, mut hi) = (0, u64::MAX);
        if let Ok(w) = std::env::var("SHARC_PCDUMP_ICOUNT") {
            let p = w
                .split_once(':')
                .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
            let Some((a, b)) = p else {
                eprintln!("SHARC_PCDUMP_ICOUNT {w:?}: want lo:hi; pc dump disabled");
                return None;
            };
            (lo, hi) = (a, b);
        }
        let left = std::env::var("SHARC_PCDUMP_LIMIT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(10000);
        let file = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("SHARC_PCDUMP_OUT {path:?}: {e}; pc dump disabled");
                return None;
            }
        };
        let mut out = std::io::BufWriter::new(file);
        let mut h = String::from("icount,pc");
        for p in ["r", "f"] {
            for i in 0..16 {
                h += &format!(",{p}{i}");
            }
        }
        for p in ["i", "m"] {
            for i in 0..16 {
                h += &format!(",{p}{i}");
            }
        }
        h += ",astatx,astaty,stkyx,mode1,mode2";
        for i in 0..16 {
            h += &format!(",s{i}");
        }
        h += ",pc_stack,loops";
        let _ = writeln!(out, "{h}");
        let mem = std::env::var("SHARC_PCDUMP_MEM").ok().and_then(|v| {
            let (a, n) = v.split_once(':')?;
            Some((
                u32::from_str_radix(a.trim_start_matches("0x"), 16).ok()?,
                n.parse().ok()?,
            ))
        });
        Some(Box::new(PcDump {
            mem,
            out,
            pcs,
            deepest: [(0, 0); 4],
            lo,
            hi,
            left,
        }))
    }

    /// Dump a row for this step if PC and icount match. UREG codes: R 0, I 16,
    /// M 32, S 80, MODE2 116, ASTATX 118, ASTATY 119, STKYX 120, MODE1 114.
    pub fn hit(&mut self, s: &crate::rt::St, pc: u64, icount: u64) {
        for (slot, n) in
            self.deepest
                .iter_mut()
                .zip([s.pc_stack.n, s.loops.n, s.status_stack.n, s.call_stack.n])
        {
            if n > slot.0 {
                *slot = (n, pc);
            }
        }
        if self.left == 0
            || icount < self.lo
            || icount > self.hi
            || !(self.pcs[0] == u64::MAX || self.pcs.contains(&pc))
        {
            return;
        }
        self.left -= 1;
        let b = |c: usize| s.r[c].b;
        let mut row = format!("{icount},{pc:#x}");
        for i in 0..16 {
            row += &format!(",{:#010x}", b(i));
        }
        for i in 0..16 {
            row += &format!(",{:e}", f32::from_bits(b(i)));
        }
        for c in (16..48).chain([118, 119, 120, 114, 116]).chain(80..96) {
            row += &format!(",{:#x}", b(c));
        }
        row += &format!(",{},{}", s.pc_stack.n, s.loops.n);
        if let Some((a, n)) = self.mem {
            for k in 0..n {
                row += &format!(",{:#x}", s.mem.read_le(a + 4 * k, 4));
            }
        }
        let _ = writeln!(self.out, "{row}");
    }
}

impl Drop for PcDump {
    fn drop(&mut self) {
        let [p, l, st, c] = self.deepest;
        let _ = writeln!(
            self.out,
            "#stack-max pc_stack={} at {:#x} loops={} at {:#x} status={} at {:#x} call={} at {:#x}",
            p.0, p.1, l.0, l.1, st.0, st.1, c.0, c.1
        );
        let _ = self.out.flush();
    }
}
