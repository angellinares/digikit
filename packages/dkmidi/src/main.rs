//! `dkmidi`: a stdin/stdout bridge to a real MIDI port, for the command-line tools that run the workbench's TypeScript session code
//! (`tools/devtools/wb.mjs`). Bytes written to stdin are assembled into SysEx messages and sent; every SysEx message received is written
//! to stdout (`F0 ... F7`, raw).
//!
//!     dkmidi ports          the port names that are an input and an output, one a line
//!     dkmidi open NAME      bridge the port; NAME is an exact name, else a unique case-insensitive part of one
use std::io::{Read, Write};
use std::process::exit;

use dkmidi::Sysex;

const USAGE: &str = "usage: dkmidi ports | dkmidi open NAME";

fn die(code: i32, msg: &str) -> ! {
    eprintln!("dkmidi: {msg}");
    exit(code);
}

/// The port `want` names: an exact match first, else the only name that contains it ignoring case.
fn resolve(want: &str, names: &[String]) -> Result<String, String> {
    if names.iter().any(|n| n == want) {
        return Ok(want.to_string());
    }
    let low = want.to_lowercase();
    let hits: Vec<&String> = names
        .iter()
        .filter(|n| n.to_lowercase().contains(&low))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no MIDI port matches {want:?} (ports: {names:?})")),
        many => Err(format!("{want:?} matches several MIDI ports: {many:?}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["ports"] => match dkmidi::ports("dkmidi") {
            Ok(names) => names.iter().for_each(|n| println!("{n}")),
            Err(e) => die(1, &e),
        },
        ["open", want] => {
            let names = dkmidi::ports("dkmidi").unwrap_or_else(|e| die(1, &e));
            let name = resolve(want, &names).unwrap_or_else(|e| die(2, &e));
            let port = dkmidi::open("dkmidi", &name).unwrap_or_else(|e| die(1, &e));
            eprintln!("open {name}");
            let rx = port.rx.clone();
            let mut out = port;
            std::thread::spawn(move || {
                let mut assembler = Sysex::default();
                let mut buf = [0u8; 4096];
                let mut stdin = std::io::stdin().lock();
                loop {
                    match stdin.read(&mut buf) {
                        Ok(0) => exit(0),
                        Ok(n) => {
                            for m in assembler.feed(&buf[..n]) {
                                if let Err(e) = out.send(&m) {
                                    die(1, &e);
                                }
                            }
                        }
                        Err(e) => die(1, &format!("stdin: {e}")),
                    }
                }
            });
            let rx = rx.lock().unwrap_or_else(|e| e.into_inner());
            let mut stdout = std::io::stdout().lock();
            while let Ok(m) = rx.recv() {
                if stdout.write_all(&m).and_then(|_| stdout.flush()).is_err() {
                    exit(0); // the reader is gone
                }
            }
            die(1, "the MIDI port closed");
        }
        _ => die(2, USAGE),
    }
}
