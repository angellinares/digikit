//! The SHARC-only rig on the command line (docs/TESTING.md "SHARC-only rig"):
//!
//!     sharc-rig REQUEST.json    (or the request on standard input)
//!
//! Writes the response JSON to standard output. The format is in `sharc_rig`.

use std::io::Read;

fn main() {
    let text = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("sharc-rig: {path}: {e}");
            std::process::exit(2);
        }),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).unwrap_or_else(|e| {
                eprintln!("sharc-rig: stdin: {e}");
                std::process::exit(2);
            });
            s
        }
    };
    match sharc_rig::run_request(&text) {
        Ok(out) => print!("{out}"),
        Err(e) => {
            eprintln!("sharc-rig: {e}");
            std::process::exit(1);
        }
    }
}
