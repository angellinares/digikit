//! The firmware's own view of a +Drive image (DT2 1.17): mount flag, directory
//! listing, name lookup, file open/size/read, called on a booted machine.
//!
//! usage: plusdrive_check [--block-read ADDR] SYX CARD.img OP...
//!
//! Boots SYX with CARD.img to ready (at most 1.5e9 instructions), then runs the
//! ops in order; one JSON line each:
//!   mounted            the mount flag `_DAT_44f2bd68`
//!   ls:/PATH           open directory `FUN_40156fc4`, `FUN_40157072` until the end
//!   read:/PATH         open `FUN_40158626`, size `FUN_4015ba22`, read `FUN_4015832e`
//!                      in 32 KiB chunks, close `FUN_401585e4`; length and SHA-256
//!   lookup:ID:NAME     name lookup `FUN_401572e0` (the 0x10000 hash index)
//!   block:SECTOR:BYTES the firmware's block read (passed with --block-read, from the port file;
//!                      arguments sector, bytes, buffer; 16-byte aligned buffer, 512 multiples):
//!                      reads BYTES rounded up to 512 and reports the first 32 bytes and the SHA-256 of BYTES
//! The addresses are DT2 1.17, matched by function hash from the DT2 1.16 functions
//! that docs/findings/14-plus-drive-format.md names (the FUN_40156fc4 etc. above are
//! the 1.16 names; docs/findings/23-digikit-runtime-spikes.md S3). Scratch memory is the free
//! RAM at 0x47f00000 (cf.expansion), the stack the one cf_call uses.

use elektron_native_boot::{CallSpec, Emulator};
use emmc_card::{Card, CardIdentity, RandomAccessRead};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};

const MOUNT_FLAG: u32 = 0x44f2_bd68;
const OPEN_DIR: u32 = 0x4015_72a0;
const NEXT_ENTRY: u32 = 0x4015_734e;
const OPEN: u32 = 0x4015_8902;
const READ: u32 = 0x4015_860a;
const CLOSE: u32 = 0x4015_88c0;
const SIZE: u32 = 0x4015_bcfe;
const LOOKUP: u32 = 0x4015_75bc;
const DT2_BLOCK_READ: u32 = 0x4012_e1b6;
const PATH: u32 = 0x47f0_0000;
const MODE: u32 = 0x47f0_0200;
const ITER: u32 = 0x47f0_1000;
const ENTRY: u32 = 0x47f0_2000;
const HANDLE: u32 = 0x47f0_3000;
const BUF: u32 = 0x47f1_0000;
const CHUNK: u32 = 0x8000;
const MAX_CALL: u64 = 200_000_000;

struct Backing {
    file: Mutex<fs::File>,
    len: u64,
}
impl RandomAccessRead for Backing {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
        let Ok(mut file) = self.file.lock() else {
            return 0;
        };
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(out))
            .map(|_| out.len())
            .unwrap_or(0)
    }
}

fn call(emu: &mut Emulator, pc: u32, args: &[u32]) -> Result<u32, String> {
    let r = emu.call_function(&CallSpec {
        pc,
        args: args.to_vec(),
        sp: 0x47fe_0000,
        ret_pc: 0x47fd_f000,
        sr: 0x2700,
        max_insns: MAX_CALL,
    })?;
    if !r.finished {
        return Err(format!("call {pc:#x} did not return: {:?}", r.error));
    }
    Ok(r.d[0])
}

fn put_str(emu: &mut Emulator, addr: u32, s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    emu.poke_bytes(addr, &bytes).unwrap();
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    let mut block_read = DT2_BLOCK_READ;
    if args.get(1).map(String::as_str) == Some("--block-read") && args.len() > 2 {
        block_read =
            u32::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("hex address");
        args.drain(1..3);
    }
    if args.len() < 4 {
        eprintln!("usage: plusdrive_check [--block-read ADDR] SYX CARD.img OP...");
        std::process::exit(2);
    }
    let syx = fs::read(&args[1]).unwrap();
    let file = fs::File::open(&args[2]).unwrap();
    let len = file.metadata().unwrap().len();
    let card = Card::with_backing(
        CardIdentity::from_env().unwrap(),
        Some(Box::new(Backing {
            file: Mutex::new(file),
            len,
        })),
    )
    .unwrap();
    let mut emu = Emulator::new(&syx, Some(card)).unwrap();
    loop {
        let snap = emu.step_chunk(250_000);
        let s = &snap.status;
        if s.ready {
            println!(
                "{}",
                json!({"op": "boot", "icount": s.icount, "main_ui": s.main_ui_reached})
            );
            break;
        }
        if s.error.is_some() || s.icount >= 1_500_000_000 {
            println!(
                "{}",
                json!({"op": "boot", "error": format!("{:?}", s.error), "icount": s.icount})
            );
            std::process::exit(3);
        }
    }
    put_str(&mut emu, MODE, "r");
    let mut failed = false;
    for op in &args[3..] {
        let (name, rest) = op.split_once(':').unwrap_or((op.as_str(), ""));
        let out = match name {
            "mounted" => json!({"op": op, "value": emu.peek32(MOUNT_FLAG)}),
            "ls" => {
                put_str(&mut emu, PATH, rest);
                match call(&mut emu, OPEN_DIR, &[PATH, ITER]) {
                    Err(e) => json!({"op": op, "error": e}),
                    Ok(rc) if rc != 0 => json!({"op": op, "open": rc as i32}),
                    Ok(_) => {
                        let mut entries = Vec::new();
                        let mut error = None;
                        for _ in 0..8192 {
                            match call(&mut emu, NEXT_ENTRY, &[ITER, ENTRY]) {
                                Ok(n) if (n as i32) > 0 => {
                                    let e = emu.peek_bytes(ENTRY, 0x114).unwrap();
                                    let id = u32::from_be_bytes(e[0..4].try_into().unwrap());
                                    let kind = u32::from_be_bytes(e[4..8].try_into().unwrap());
                                    entries.push(
                                        json!({"id": id, "type": kind, "name": cstr(&e[0x14..])}),
                                    );
                                }
                                Ok(_) => break,
                                Err(e) => {
                                    error = Some(e);
                                    break;
                                }
                            }
                        }
                        json!({"op": op, "open": 0, "entries": entries, "error": error})
                    }
                }
            }
            "read" => {
                put_str(&mut emu, PATH, rest);
                match call(&mut emu, OPEN, &[PATH, MODE, HANDLE]) {
                    Err(e) => json!({"op": op, "error": e}),
                    Ok(rc) if rc != 0 => json!({"op": op, "open": rc as i32}),
                    Ok(_) => {
                        let id = u32::from_be_bytes(
                            emu.peek_bytes(HANDLE, 4).unwrap().try_into().unwrap(),
                        );
                        let size = call(&mut emu, SIZE, &[id]);
                        let (mut total, mut sha, mut error) = (0u64, Sha256::new(), None);
                        loop {
                            match call(&mut emu, READ, &[BUF, CHUNK, HANDLE]) {
                                Ok(n) if (n as i32) > 0 => {
                                    sha.update(emu.peek_bytes(BUF, n as usize).unwrap());
                                    total += u64::from(n);
                                }
                                Ok(n) if n == 0 => break,
                                Ok(n) => {
                                    error = Some(format!("read returned {}", n as i32));
                                    break;
                                }
                                Err(e) => {
                                    error = Some(e);
                                    break;
                                }
                            }
                        }
                        let close = call(&mut emu, CLOSE, &[HANDLE]);
                        let digest: String =
                            sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
                        json!({"op": op, "open": 0, "id": id, "size": size.ok(), "read": total,
                               "sha256": digest, "close": close.ok(), "error": error})
                    }
                }
            }
            "lookup" => {
                let (dir, entry_name) = rest.split_once(':').unwrap_or(("2", rest));
                put_str(&mut emu, PATH, entry_name);
                let dir: u32 = dir.parse().unwrap();
                match call(&mut emu, LOOKUP, &[dir, PATH, ENTRY]) {
                    Err(e) => json!({"op": op, "error": e}),
                    Ok(found) => {
                        let e = emu.peek_bytes(ENTRY, 0x114).unwrap();
                        json!({"op": op, "found": found, "id": u32::from_be_bytes(e[0..4].try_into().unwrap()),
                               "name": cstr(&e[0x14..])})
                    }
                }
            }
            "block" => {
                let (sector, bytes) = rest.split_once(':').unwrap_or((rest, "512"));
                let sector = u32::from_str_radix(sector.trim_start_matches("0x"), 16).unwrap();
                let bytes: usize = bytes.parse().unwrap();
                let (mut data, mut at, mut rc_all) = (Vec::new(), 0usize, 0u32);
                let mut error = None;
                while at < bytes {
                    let n = (bytes - at).div_ceil(512).min((CHUNK / 512) as usize) * 512;
                    match call(
                        &mut emu,
                        block_read,
                        &[sector + (at / 512) as u32, n as u32, BUF],
                    ) {
                        Ok(rc) => {
                            rc_all |= rc;
                            data.extend_from_slice(&emu.peek_bytes(BUF, n).unwrap());
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                    at += n;
                }
                data.truncate(bytes);
                let digest: String = Sha256::digest(&data)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let head: String = data.iter().take(32).map(|b| format!("{b:02x}")).collect();
                json!({"op": op, "rc": rc_all, "read": data.len(), "sha256": digest, "head": head, "error": error})
            }
            other => json!({"op": op, "error": format!("unknown op {other}")}),
        };
        failed |= out.get("error").is_some_and(|e| !e.is_null());
        println!("{out}");
    }
    std::process::exit(i32::from(failed));
}
