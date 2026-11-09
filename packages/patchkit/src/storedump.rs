//! `patchkit store dump`: read digikit's own data from a card image, a raw sector dump, or a settings record file, and print it (text, or `--json`).
//!
//! What it shows (docs/design/digikit-store.md): the store header (the copy in use, its sequence number, the sectors used), the file table, the selection
//! record (both copies and the one the runtime uses), and the settings record (both copies, the valid one with the higher sequence number, every entry
//! decoded; the keys of `sdk/sdk.toml [settings.key]` are printed as what they hold, for example the 16 send amounts of an effect as 16 numbers).
//! The settings decoder is `dkrt_sdk::settings`, the code the runtime uses; `sdk/vectors/settings-vectors.json` checks it and the Python reader.
//!
//! The card is a regular file (a card image of the emulator, sparse, 512-byte sectors), optionally with a `card.overlay` file of the guest's writes laid
//! over it (as `plusdrive export --overlay`), or a raw dump of part of a card whose first byte is the sector `--base-sector N`. `--record FILE` reads one
//! settings record (the bytes `tools/device/dkupload.py settings -o FILE` saves from a device) and needs no card.
use dkrt_sdk::settings as set;
use emmc_card::RandomAccessRead;
use plusdrive_format::export::FileImage;
use plusdrive_format::{
    OverlayImage, Selection, card_layout, current_selection, decode_selection, parse_overlay,
    read_store,
};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;

pub const USAGE: &str = "usage: patchkit store dump IMAGE [--device dt2|dn2] [--overlay FILE] [--base-sector N] [--json]
       patchkit store dump --record FILE [--json]

  IMAGE           a card image (a regular file of 512-byte sectors; sparse is fine), or a raw dump of part of a card (--base-sector)
  --device        the card layout of dt2 (default) or dn2: where the store, the settings record and the selection record are
  --overlay FILE  a `card.overlay` file (the guest's writes of an emulator run), laid over IMAGE
  --base-sector N the first byte of IMAGE is card sector N (a dump that starts at the store: N = the device's store `first_sector`)
  --record FILE   one settings record, as `dkupload settings -o FILE` saves it (the active copy's bytes), instead of a card
  --json          machine-readable output";

/// A raw dump seen as a card: the file's first byte is sector `base`; before it the card reads as zeros.
struct Window<'a> {
    inner: &'a dyn RandomAccessRead,
    base_bytes: u64,
}

impl RandomAccessRead for Window<'_> {
    fn len(&self) -> u64 {
        self.inner.len() + self.base_bytes
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
        out.fill(0);
        if offset + out.len() as u64 <= self.base_bytes {
            return out.len();
        }
        if offset < self.base_bytes {
            // a read that straddles the start: the part before it stays zero
            let skip = (self.base_bytes - offset) as usize;
            let n = self.inner.read_at(0, &mut out[skip..]);
            return skip + n;
        }
        self.inner.read_at(offset - self.base_bytes, out)
    }
}

/// One entry of a settings record, decoded.
struct Entry {
    key: String,
    value: Vec<u8>,
    /// `u8`, `route`, `u8x16` (from `sdk.toml`), or empty for a key that no owner's prefix covers.
    kind: &'static str,
}

/// One copy of the settings record as the card holds it.
struct Copy {
    name: &'static str,
    sector: u64,
    /// `ok`, or the first rule the record breaks (`set::why`), or `empty` when the copy holds nothing of ours.
    state: &'static str,
    seq: u32,
    used: usize,
    entries: Vec<Entry>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The words a host reader prints for a value: `(text, json)`.
fn decode_value(kind: &str, v: &[u8]) -> (String, Value) {
    if kind == "route" && v.len() == 1 && v[0] <= 1 {
        let w = if v[0] == 0 { "master" } else { "send" };
        return (format!("{w} ({})", v[0]), json!(w));
    }
    if kind == "u8" && v.len() == 1 {
        return (format!("{}", v[0]), json!(v[0]));
    }
    if kind == "u8x16" && v.len() == 16 {
        let list: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        return (format!("[{}]", list.join(", ")), json!(v));
    }
    if !v.is_empty() && v.iter().all(|c| (0x20..0x7f).contains(c)) {
        let s = String::from_utf8_lossy(v).into_owned();
        return (format!("{s:?}"), json!(s));
    }
    (format!("0x{}", hex(v)), Value::Null)
}

fn read_copy(img: &dyn RandomAccessRead, name: &'static str, sector: u64) -> Copy {
    let mut c = Copy {
        name,
        sector,
        state: "empty",
        seq: 0,
        used: 0,
        entries: vec![],
    };
    let mut first = [0u8; 512];
    img.read_at(sector * 512, &mut first);
    let Some((seq, used)) = set::peek(&first) else {
        // the magic or the version is wrong: nothing of ours (an all-zero sector is a copy never written)
        c.state = if first.iter().all(|&b| b == 0) {
            "empty"
        } else {
            set::why(&first)
        };
        return c;
    };
    c.seq = seq;
    c.used = used;
    let sectors = used.div_ceil(512);
    let mut buf = vec![0u8; sectors * 512];
    img.read_at(sector * 512, &mut buf);
    c.state = set::why(&buf[..used.max(set::HEADER).min(buf.len())]);
    if c.state == "ok" {
        c.entries = set::iter(&buf, used)
            .map(|(k, v)| {
                let kind = set::kind_of(k).unwrap_or("");
                Entry {
                    key: String::from_utf8_lossy(k).into_owned(),
                    value: v.to_vec(),
                    kind,
                }
            })
            .collect();
    }
    c
}

/// The copy the runtime uses: the valid one with the higher sequence number, A when equal.
fn active(a: &Copy, b: &Copy) -> Option<usize> {
    match (a.state == "ok", b.state == "ok") {
        (true, true) => Some(if b.seq > a.seq { 1 } else { 0 }),
        (true, false) => Some(0),
        (false, true) => Some(1),
        _ => None,
    }
}

fn copy_json(c: &Copy) -> Value {
    json!({
        "copy": c.name, "sector": c.sector, "state": c.state, "seq": c.seq, "used": c.used,
        "entries": c.entries.iter().map(|e| json!({
            "key": e.key, "kind": e.kind, "value_hex": hex(&e.value), "value": decode_value(e.kind, &e.value).1,
        })).collect::<Vec<_>>(),
    })
}

fn copy_text(out: &mut String, c: &Copy, is_active: bool) {
    let mark = if is_active { "  <- in use" } else { "" };
    if c.state == "ok" {
        let _ = writeln!(
            out,
            "  copy {}: sector {:#x}, valid, seq {}, {} bytes, {} entries{mark}",
            c.name,
            c.sector,
            c.seq,
            c.used,
            c.entries.len()
        );
    } else if c.state == "empty" {
        let _ = writeln!(
            out,
            "  copy {}: sector {:#x}, empty (never written)",
            c.name, c.sector
        );
    } else {
        let _ = writeln!(
            out,
            "  copy {}: sector {:#x}, NOT valid ({}), seq {}",
            c.name, c.sector, c.state, c.seq
        );
    }
}

fn entries_text(out: &mut String, c: &Copy) {
    for e in &c.entries {
        let (text, _) = decode_value(e.kind, &e.value);
        let _ = writeln!(out, "    {}  {}", e.key, text);
    }
}

fn sel_json(c: Option<&Selection>) -> Value {
    match c {
        None => Value::Null,
        Some(s) => {
            json!({"seq": s.seq, "boot_counter": s.boot_counter, "flags": s.flags, "paths": s.paths})
        }
    }
}

fn dump_record(path: &str, json_out: bool) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let why = set::why(&bytes);
    let (seq, used) = set::peek(&bytes).unwrap_or((0, 0));
    let entries: Vec<Entry> = if why == "ok" {
        set::iter(&bytes, used)
            .map(|(k, v)| Entry {
                key: String::from_utf8_lossy(k).into_owned(),
                value: v.to_vec(),
                kind: set::kind_of(k).unwrap_or(""),
            })
            .collect()
    } else {
        vec![]
    };
    let c = Copy {
        name: "-",
        sector: 0,
        state: why,
        seq,
        used,
        entries,
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"record": path, "settings": copy_json(&c)}))
                .unwrap()
        );
    } else {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "settings record {path}: {}, seq {}, {} bytes, {} entries",
            if why == "ok" { "valid" } else { "NOT valid" },
            seq,
            used,
            c.entries.len()
        );
        if why != "ok" {
            let _ = writeln!(out, "  refused: {why}");
        }
        entries_text(&mut out, &c);
        print!("{out}");
    }
    if why == "ok" {
        Ok(())
    } else {
        Err(format!("{path}: not a valid settings record ({why})"))
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (mut image, mut device, mut overlay, mut base, mut record) =
        (None, String::from("dt2"), None, 0u64, None);
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--device" => device = it.next().ok_or("missing --device value")?.clone(),
            "--overlay" => {
                overlay = Some(PathBuf::from(it.next().ok_or("missing --overlay value")?))
            }
            "--base-sector" => {
                let v = it.next().ok_or("missing --base-sector value")?;
                base = match v.strip_prefix("0x") {
                    Some(h) => u64::from_str_radix(h, 16),
                    None => v.parse(),
                }
                .map_err(|_| "invalid --base-sector".to_string())?;
            }
            "--record" => record = Some(it.next().ok_or("missing --record value")?.clone()),
            "--json" => json_out = true,
            _ if image.is_none() && !a.starts_with('-') => image = Some(PathBuf::from(a)),
            _ => return Err(USAGE.into()),
        }
    }
    if let Some(r) = record {
        if image.is_some() {
            return Err(USAGE.into());
        }
        return dump_record(&r, json_out);
    }
    let image = image.ok_or(USAGE)?;
    let file = FileImage::open(&image)?;
    let window = Window {
        inner: &file,
        base_bytes: base * 512,
    };
    let over;
    let overlaid;
    let img: &dyn RandomAccessRead = if let Some(path) = &overlay {
        over = parse_overlay(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
            .map_err(|e| format!("{}: {e:?}", path.display()))?;
        overlaid = OverlayImage::new(&window, &over);
        &overlaid
    } else {
        &window
    };
    let layout = card_layout(&device)?;
    let range = layout.store.ok_or("this device has no module store")?;
    let half = set::SECTORS as u64;
    if range.settings_sectors != 2 * half {
        return Err(format!(
            "the device file's settings_sectors is {}, sdk/sdk.toml [settings] says {}",
            range.settings_sectors,
            2 * half
        ));
    }
    // the store header and the table
    let size = range.store_limit();
    let store = read_store(img, range.first_sector, size, range.header_b());
    // the selection record
    let sel = |sector: u64| {
        let mut b = [0u8; 512];
        img.read_at(sector * 512, &mut b);
        decode_selection(&b)
    };
    let (sa, sb) = (sel(range.end_sector - 2), sel(range.end_sector - 1));
    let cur = current_selection(sa.clone(), sb.clone());
    let sel_active = match (&sa, &sb) {
        (Some(a), Some(b)) => Some(if b.seq > a.seq { "B" } else { "A" }),
        (Some(_), None) => Some("A"),
        (None, Some(_)) => Some("B"),
        _ => None,
    };
    // the settings record
    let ca = read_copy(img, "A", range.settings_a());
    let cb = read_copy(img, "B", range.settings_a() + half);
    let act = active(&ca, &cb);

    if json_out {
        let store_json = match &store {
            Ok(Some(s)) => json!({
                "version": s.version, "seq": s.seq, "copy": if s.from_b { "B" } else { "A" }, "used_sectors": s.used_sectors, "limit_sectors": size,
                "files": s.entries.iter().map(|e| json!({
                    "path": e.path, "first_sector": e.first_sector, "size": e.size_bytes, "hash": hex(&e.hash), "folder": e.is_dir,
                })).collect::<Vec<_>>(),
            }),
            Ok(None) => Value::Null,
            Err(e) => json!({"error": format!("{e:?}")}),
        };
        let v = json!({
            "device": device,
            "area": {"first_sector": range.first_sector, "end_sector": range.end_sector, "header_b": range.header_b(),
                     "settings_a": range.settings_a(), "settings_b": range.settings_a() + half, "selection_a": range.end_sector - 2, "selection_b": range.end_sector - 1},
            "store": store_json,
            "selection": {"a": sel_json(sa.as_ref()), "b": sel_json(sb.as_ref()), "active": sel_active},
            "settings": {"active": act.map(|i| if i == 0 { "A" } else { "B" }), "copies": [copy_json(&ca), copy_json(&cb)]},
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(());
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "card {device}: store sectors {:#x}..{:#x}, header B {:#x}, settings {:#x} and {:#x}, selection {:#x} and {:#x}",
        range.first_sector,
        range.end_sector,
        range.header_b(),
        range.settings_a(),
        range.settings_a() + half,
        range.end_sector - 2,
        range.end_sector - 1
    );
    match &store {
        Ok(Some(s)) => {
            let _ = writeln!(
                out,
                "store: header {} in use, version {}, seq {}, {} of {} sectors used, {} entries",
                if s.from_b { "B" } else { "A" },
                s.version,
                s.seq,
                s.used_sectors,
                size,
                s.entries.len()
            );
            let mut es: Vec<_> = s.entries.iter().collect();
            es.sort_by(|a, b| a.path.cmp(&b.path));
            for e in es {
                if e.is_dir {
                    let _ = writeln!(out, "  {}/  folder", e.path);
                } else {
                    let _ = writeln!(
                        out,
                        "  {}  {} bytes  first sector {:#x}  hash {}",
                        e.path,
                        e.size_bytes,
                        e.first_sector,
                        hex(&e.hash)
                    );
                }
            }
        }
        Ok(None) => out.push_str("store: no header (nothing was built into the area)\n"),
        Err(e) => {
            let _ = writeln!(out, "store: damaged ({e:?})");
        }
    }
    let fmt = |s: &Option<Selection>| match s {
        Some(s) => format!(
            "valid, seq {}, boot counter {}, flags {}, {} modules",
            s.seq,
            s.boot_counter,
            s.flags,
            s.paths.len()
        ),
        None => "empty or not valid".to_string(),
    };
    let _ = writeln!(out, "selection: copy A {}; copy B {}", fmt(&sa), fmt(&sb));
    if let Some(c) = &cur {
        for p in &c.paths {
            let _ = writeln!(out, "  {p}");
        }
    }
    let _ = writeln!(
        out,
        "settings: {}",
        match act {
            Some(0) => "copy A in use",
            Some(_) => "copy B in use",
            None => "no valid copy (an empty record)",
        }
    );
    copy_text(&mut out, &ca, act == Some(0));
    copy_text(&mut out, &cb, act == Some(1));
    if let Some(i) = act {
        let c = if i == 0 { &ca } else { &cb };
        let _ = writeln!(out, "  entries of copy {} (seq {}):", c.name, c.seq);
        entries_text(&mut out, c);
    }
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../../sdk/vectors/settings-vectors.json")).unwrap()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    /// The copy reader gives what the vectors say, from a card that holds the record at the settings sector.
    #[test]
    fn the_copy_reader_agrees_with_the_shared_vectors() {
        struct Mem(Vec<u8>);
        impl RandomAccessRead for Mem {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_at(&self, off: u64, out: &mut [u8]) -> usize {
                let off = off as usize;
                out.fill(0);
                let n = out.len().min(self.0.len().saturating_sub(off));
                out[..n].copy_from_slice(&self.0[off..off + n]);
                n
            }
        }
        let v = vectors();
        for c in v["valid"].as_array().unwrap() {
            let mut card = vec![0u8; 4 * 512];
            let rec = unhex(c["hex"].as_str().unwrap());
            card[512..512 + rec.len()].copy_from_slice(&rec);
            let copy = read_copy(&Mem(card), "A", 1);
            assert_eq!(copy.state, "ok", "{}", c["name"]);
            assert_eq!(copy.seq as u64, c["seq"].as_u64().unwrap());
            assert_eq!(copy.used as u64, c["used"].as_u64().unwrap());
            let want: Vec<(String, String)> = c["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e[0].as_str().unwrap().to_string(),
                        e[1].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            let got: Vec<(String, String)> = copy
                .entries
                .iter()
                .map(|e| (e.key.clone(), hex(&e.value)))
                .collect();
            assert_eq!(got, want, "{}", c["name"]);
        }
        for c in v["invalid"].as_array().unwrap() {
            let mut card = vec![0u8; 20 * 512];
            let rec = unhex(c["hex"].as_str().unwrap());
            card[512..512 + rec.len()].copy_from_slice(&rec);
            let copy = read_copy(&Mem(card), "A", 1);
            assert_ne!(copy.state, "ok", "{}", c["name"]);
            assert!(copy.entries.is_empty());
        }
        assert_eq!(read_copy(&Mem(vec![0; 2048]), "B", 1).state, "empty");
    }

    #[test]
    fn values_print_as_what_they_hold() {
        assert_eq!(decode_value("route", &[1]).0, "send (1)");
        assert_eq!(decode_value("route", &[0]).0, "master (0)");
        assert_eq!(decode_value("route", &[7]).0, "0x07");
        let amounts = [0u8, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100];
        assert_eq!(
            decode_value("u8x16", &amounts).0,
            "[0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100]"
        );
        assert_eq!(decode_value("u8x16", &amounts[..3]).0, "0x007f40");
        assert_eq!(decode_value("", b"hello").0, "\"hello\"");
    }

    #[test]
    fn the_active_copy_is_the_valid_one_with_the_higher_seq() {
        let c = |state, seq| Copy {
            name: "-",
            sector: 0,
            state,
            seq,
            used: 0,
            entries: vec![],
        };
        assert_eq!(active(&c("ok", 2), &c("ok", 3)), Some(1));
        assert_eq!(active(&c("ok", 3), &c("ok", 3)), Some(0));
        assert_eq!(active(&c("ok", 2), &c("crc", 9)), Some(0));
        assert_eq!(active(&c("empty", 0), &c("crc", 9)), None);
    }
}
