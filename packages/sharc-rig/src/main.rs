//! The SHARC-only rig on the command line (docs/TESTING.md "SHARC-only rig"):
//!
//!     sharc-rig REQUEST.json    (or the request on standard input)
//!     sharc-rig --serve         (a request line, then `{"ops": [...]}` lines, on standard input)
//!
//! Writes the response JSON to standard output (one line per input line with --serve). The format is in `sharc_rig`.

use std::io::{BufRead, Read, Write};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--serve") {
        let stdin = std::io::stdin();
        let mut out = std::io::stdout().lock();
        let lines = stdin.lock().lines().map_while(Result::ok);
        // flush per line: the caller waits for the answer before it sends the next line
        let result = sharc_rig::serve(lines, |text| {
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        });
        if let Err(e) = result {
            eprintln!("sharc-rig: {e}");
            std::process::exit(1);
        }
        return;
    }
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
