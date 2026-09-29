//! Decode instructions of a raw big-endian image, for the oracle comparison
//! (tools/cfisa/oracle.py). Reads hex addresses from stdin, one per line, and
//! prints one line per address:
//!
//!     <addr> <len> <form> <text>        decoded
//!     <addr> 0 - illegal                 not a legal encoding
//!
//! Usage: cfdis IMAGE BASE_HEX < addresses
//!        cfdis --forms                  list the table's forms (id unit privileged page)

use std::io::{BufRead, BufWriter, Write};

use coldfire::decode::{FORM_IDS, PAGES, PRIVILEGED, UNITS};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    if args.len() == 2 && args[1] == "--forms" {
        for i in 0..FORM_IDS.len() {
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                FORM_IDS[i], UNITS[i], PRIVILEGED[i], PAGES[i]
            )
            .unwrap();
        }
        return;
    }
    if args.len() == 2 && args[1] == "--words" {
        // "<pc> <w0> <w1> <w2>" in hex per line: decode those words at pc.
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.unwrap();
            let v: Vec<u32> = line
                .split_whitespace()
                .map(|t| u32::from_str_radix(t, 16).expect("hex"))
                .collect();
            if v.len() != 4 {
                continue;
            }
            match coldfire::decode(v[0], [v[1] as u16, v[2] as u16, v[3] as u16]) {
                Some(i) => writeln!(out, "{:08x} {} {} {}", v[0], i.len, i.id(), i).unwrap(),
                None => writeln!(out, "{:08x} 0 - illegal", v[0]).unwrap(),
            }
        }
        return;
    }
    if args.len() != 3 {
        eprintln!(
            "usage: cfdis IMAGE BASE_HEX < addresses | cfdis --words < 'pc w0 w1 w2' | cfdis --forms"
        );
        std::process::exit(2);
    }
    let image = std::fs::read(&args[1]).expect("read image");
    let base = u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("base");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let addr = u32::from_str_radix(t.trim_start_matches("0x"), 16).expect("address");
        let off = addr.wrapping_sub(base) as usize;
        match coldfire::decode_at(&image, base, off) {
            Some(i) => writeln!(out, "{:08x} {} {} {}", addr, i.len, i.id(), i).unwrap(),
            None => writeln!(out, "{:08x} 0 - illegal", addr).unwrap(),
        }
    }
}
