//! The host `Session` against a real device in the emulator: a transport over the emulator's control socket
//! (`midi.usb.in`, `midi.usb.out`, `step`), the same calls as the desktop Transfer page's transport (apps/desktop/ui/src/desktop-transport.ts).
//! Time in the emulator passes only when stepped, so `recv` steps it until a reply is there.
//!
//! ```text
//! cargo run -p dkupload --example emu_session -- SOCKET [--chunk N] [--period N] [--drop N] [--budget N] [--depth-one] CMD ARGS
//!   info | ls [PREFIX] | mkdir PATH | verify PATH [OFFSET LENGTH] | put FILE PATH | rm PATH | clear | badhash FILE PATH
//! ```
//!
//! One JSON object per line on stdout, with `stepped` (emulated instructions stepped while waiting). `--drop N` throws away
//! the Nth reply the device sent (a lost reply); `--budget` is the instructions one `recv` may step before it gives up.
//! The kept proof is the `upload-rust` scenario (tests/digikit, tools/patchkit/pk_digikit_upload.py, part `rust`).
use dkupload::codec::{CMD_BEGIN, CMD_DATA, CMD_END};
use dkupload::{Error, Session, Transport, codec, proto};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

struct Emu {
    w: UnixStream,
    r: BufReader<UnixStream>,
    id: u64,
}

impl Emu {
    fn connect(path: &str) -> Result<Emu, String> {
        let s = UnixStream::connect(path).map_err(|e| format!("{path}: {e}"))?;
        let mut e = Emu {
            w: s.try_clone().map_err(|e| e.to_string())?,
            r: BufReader::new(s),
            id: 0,
        };
        e.call("hello", json!({"min_schema": 1}))?;
        Ok(e)
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.id += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        writeln!(self.w, "{req}").map_err(|e| e.to_string())?;
        loop {
            let mut line = String::new();
            if self.r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("control socket closed".into());
            }
            let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if v["id"].as_u64() != Some(self.id) {
                continue; // an event
            }
            if let Some(err) = v.get("error") {
                return Err(err["message"].as_str().unwrap_or("error").to_string());
            }
            return Ok(v["result"].clone());
        }
    }
}

struct EmuTransport {
    emu: Emu,
    rx: Vec<u8>,
    chunk: Option<u64>,
    period: Option<u64>,
    drop_nth: Option<u32>,
    seen: u32,
    step: u64,
    budget: u64,
    stepped: u64,
}

impl EmuTransport {
    fn take_message(&mut self) -> Option<Vec<u8>> {
        let Some(start) = self.rx.iter().position(|&b| b == 0xF0) else {
            self.rx.clear();
            return None;
        };
        self.rx.drain(..start);
        let end = self.rx.iter().position(|&b| b == 0xF7)?;
        Some(self.rx.drain(..=end).collect())
    }
}

fn from_hex(h: &str) -> Vec<u8> {
    h.as_bytes()
        .chunks(2)
        .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect()
}

impl Transport for EmuTransport {
    fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        let mut p = json!({"bytes": proto::hex(msg)});
        if let Some(c) = self.chunk {
            p["chunk"] = c.into();
        }
        if let Some(c) = self.period {
            p["period"] = c.into();
        }
        self.emu.call("midi.usb.in", p).map(drop)
    }

    fn recv(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        let mut spent = 0;
        loop {
            let out = self.emu.call("midi.usb.out", json!({}))?;
            self.rx.extend(from_hex(out["hex"].as_str().unwrap_or("")));
            while let Some(m) = self.take_message() {
                self.seen += 1;
                if self.drop_nth == Some(self.seen) {
                    continue; // a lost reply
                }
                return Ok(Some(m));
            }
            if spent >= self.budget {
                return Ok(None);
            }
            self.emu.call("step", json!({"instructions": self.step}))?;
            spent += self.step;
            self.stepped += self.step;
        }
    }
}

fn entries(list: Vec<proto::Entry>) -> Value {
    list.into_iter()
        .map(|e| json!({"path": e.path, "size": e.size, "hash": proto::hex(&e.hash)}))
        .collect()
}

fn err_json(e: &Error) -> Value {
    match e {
        Error::Device { status, free_bytes } => {
            json!({"error": e.to_string(), "kind": "device", "status": status.code(), "name": status.name(), "free_bytes": free_bytes})
        }
        Error::Timeout => json!({"error": e.to_string(), "kind": "timeout"}),
        _ => json!({"error": e.to_string(), "kind": "other"}),
    }
}

/// A put with a wrong file hash in BEGIN: the same requests `Session::put` sends, so END answers E_HASH.
fn bad_hash(s: &mut Session<EmuTransport>, path: &str, data: &[u8]) -> Result<(), Error> {
    let mut wrong = codec::file_hash(data);
    wrong[0] ^= 0xFF;
    s.request(
        CMD_BEGIN,
        &proto::begin_body(path, data.len() as u32, &wrong)?,
    )?;
    for (i, c) in data.chunks(codec::MAX_CHUNK).enumerate() {
        s.request(
            CMD_DATA,
            &proto::data_body((i * codec::MAX_CHUNK) as u32, c),
        )?;
    }
    s.request(CMD_END, &[]).map(drop)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!(
            "usage: emu_session SOCKET [--chunk N] [--period N] [--drop N] [--budget N] CMD ARGS"
        );
        std::process::exit(2);
    }
    let sock = args.remove(0);
    let mut t = EmuTransport {
        emu: Emu::connect(&sock).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2)
        }),
        rx: Vec::new(),
        chunk: None,
        period: None,
        drop_nth: None,
        seen: 0,
        step: 400_000,
        budget: 4_000_000_000,
        stepped: 0,
    };
    while args.first().is_some_and(|a| a.starts_with("--")) {
        let flag = args.remove(0);
        let v: u64 = args.remove(0).parse().expect("a number");
        match flag.as_str() {
            "--chunk" => t.chunk = Some(v),
            "--period" => t.period = Some(v),
            "--drop" => t.drop_nth = Some(v as u32),
            "--budget" => t.budget = v,
            _ => panic!("unknown flag {flag}"),
        }
    }
    let mut s = Session::new(t);
    s.timeout = Duration::from_secs(6);
    let t0 = Instant::now();
    let cmd = args.first().cloned().unwrap_or_default();
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let result: Result<Value, Error> = match cmd.as_str() {
        "info" => s.info().map(|i| {
            json!({"protocol": i.protocol, "runtime_version": i.runtime_version, "port": i.port, "free_bytes": i.free_bytes,
                   "max_chunk": i.max_chunk, "safe_mode": i.safe_mode(), "upload_open": i.upload_open()})
        }),
        "ls" if args.len() > 1 => s.list_prefix(&arg(1)).map(entries),
        "ls" => s.list().map(entries),
        "mkdir" => s.mkdir(&arg(1)).map(|_| json!({})),
        "verify" => {
            let (off, len) = (arg(2).parse::<u32>(), arg(3).parse::<u32>());
            match (off, len) {
                (Ok(o), Ok(l)) => s.verify_range(&arg(1), o, l),
                _ => s.verify(&arg(1)),
            }
            .map(|sha| json!(proto::hex(&sha)))
        }
        "rm" => s.delete(&arg(1)).map(|_| json!({})),
        "clear" => s.clear().map(|_| json!({})),
        "put" | "badhash" => {
            let data = std::fs::read(arg(1)).expect("file");
            let path = arg(2);
            let r = if cmd == "put" {
                s.put(&path, data, &mut |_| true)
            } else {
                bad_hash(&mut s, &path, &data)
            };
            r.map(|_| json!({}))
        }
        _ => {
            eprintln!("unknown command {cmd}");
            std::process::exit(2)
        }
    };
    let mut out = match result {
        Ok(v) => json!({"ok": true, "result": v}),
        Err(e) => json!({"ok": false, "failure": err_json(&e)}),
    };
    out["stepped"] = s.transport.stepped.into();
    out["replies_seen"] = s.transport.seen.into();
    out["wall_s"] = (t0.elapsed().as_secs_f64()).into();
    println!("{out}");
}
