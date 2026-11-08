//! The settings record: the shared vectors, the edits, the limits, and the two copies on a card in RAM (which one is read, a power cut in a write).
extern crate std;
use super::*;
use std::string::String;
use std::sync::Mutex;
use std::vec::Vec;

const FIRST: u32 = 5000;
const SECTORS: u32 = 4096;
const LAY: Layout = Layout {
    first: FIRST,
    sectors: SECTORS,
};

static CARD: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// The one read that fails (counted from 0 since `fresh`).
static FAIL_ONE: Mutex<Option<u32>> = Mutex::new(None);
static LOCK: Mutex<()> = Mutex::new(());
/// (reads that fail from the n-th on, writes that fail from the n-th on); counts of reads and writes done.
static FAIL: Mutex<(u32, u32, u32, u32)> = Mutex::new((u32::MAX, u32::MAX, 0, 0));
/// Sectors written by the last writes: (sector, count).
static WRITES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "C" fn rd(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    {
        let mut f = FAIL.lock().unwrap_or_else(|e| e.into_inner());
        let n = f.2;
        f.2 += 1;
        if n >= f.0 || *FAIL_ONE.lock().unwrap_or_else(|e| e.into_inner()) == Some(n) {
            return -1;
        }
    }
    let c = CARD.lock().unwrap();
    let at = (sector - FIRST) as usize * 512;
    core::ptr::copy_nonoverlapping(c.as_ptr().add(at), buf, bytes as usize);
    0
}

unsafe extern "C" fn wr(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    {
        let mut f = FAIL.lock().unwrap_or_else(|e| e.into_inner());
        if f.3 >= f.1 {
            return -1;
        }
        f.3 += 1;
    }
    assert!(
        LAY.write_allowed(sector, bytes),
        "a write outside the store: {sector} {bytes}"
    );
    // only the settings area, and only the copy that was asked for
    assert!(
        sector >= LAY.set_a() && sector + bytes / 512 <= LAY.sel_a(),
        "a write outside the settings area: {sector}"
    );
    WRITES.lock().unwrap().push((sector, bytes / 512));
    let mut c = CARD.lock().unwrap();
    let at = (sector - FIRST) as usize * 512;
    core::ptr::copy_nonoverlapping(buf, c.as_mut_ptr().add(at), bytes as usize);
    0
}

const IO: Io = Io { rd, wr };

#[repr(C, align(16))]
struct Area([u8; AREA]);
#[repr(C, align(16))]
struct Sec([u8; 512]);

fn fresh() {
    *CARD.lock().unwrap_or_else(|e| e.into_inner()) = std::vec![0u8; SECTORS as usize * 512];
    *FAIL.lock().unwrap_or_else(|e| e.into_inner()) = (u32::MAX, u32::MAX, 0, 0);
    *FAIL_ONE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    WRITES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn area_of(b: &[u8]) -> Box<Area> {
    let mut a = Box::new(Area([0; AREA]));
    a.0[..b.len()].copy_from_slice(b);
    a
}

use std::boxed::Box;

fn new_record(seq: u32) -> Box<Area> {
    let mut a = Box::new(Area([0; AREA]));
    init(&mut a.0, seq);
    a
}

fn load_now() -> (Result<Loaded, i32>, Box<Area>) {
    let mut a = Box::new(Area([0xee; AREA]));
    let mut h = Sec([0; 512]);
    let r = unsafe { load(&IO, &LAY, h.0.as_mut_ptr(), &mut a.0) };
    (r, a)
}

fn entries(a: &Area) -> Vec<(Vec<u8>, Vec<u8>)> {
    let u = used(&a.0);
    iter(&a.0, u)
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect()
}

/// A minimal JSON reader (the crate has no dependency: `rustc --test` builds it alone): objects, arrays, strings without escapes, unsigned numbers.
#[derive(Debug)]
enum Value {
    Null,
    Num(u64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

static NULL: Value = Value::Null;

impl Value {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
}

impl core::ops::Index<&str> for Value {
    type Output = Value;
    fn index(&self, k: &str) -> &Value {
        match self {
            Value::Obj(o) => o
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v)
                .unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

impl core::ops::Index<usize> for Value {
    type Output = Value;
    fn index(&self, i: usize) -> &Value {
        match self {
            Value::Arr(a) => a.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.at < self.b.len() && self.b[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }
    fn eat(&mut self, c: u8) {
        self.ws();
        assert_eq!(
            self.b[self.at], c,
            "json: expected {} at {}",
            c as char, self.at
        );
        self.at += 1;
    }
    fn string(&mut self) -> String {
        self.eat(b'"');
        let start = self.at;
        while self.b[self.at] != b'"' {
            assert_ne!(self.b[self.at], b'\\', "json: escapes are not read");
            self.at += 1;
        }
        let s = String::from_utf8(self.b[start..self.at].to_vec()).unwrap();
        self.at += 1;
        s
    }
    fn value(&mut self) -> Value {
        self.ws();
        match self.b[self.at] {
            b'{' => {
                self.at += 1;
                let mut o = Vec::new();
                loop {
                    self.ws();
                    if self.b[self.at] == b'}' {
                        self.at += 1;
                        return Value::Obj(o);
                    }
                    if !o.is_empty() {
                        self.eat(b',');
                    }
                    let k = self.string();
                    self.eat(b':');
                    o.push((k, self.value()));
                }
            }
            b'[' => {
                self.at += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    if self.b[self.at] == b']' {
                        self.at += 1;
                        return Value::Arr(a);
                    }
                    if !a.is_empty() {
                        self.eat(b',');
                    }
                    a.push(self.value());
                }
            }
            b'"' => Value::Str(self.string()),
            b'n' => {
                self.at += 4;
                Value::Null
            }
            _ => {
                let start = self.at;
                while self.at < self.b.len() && self.b[self.at].is_ascii_digit() {
                    self.at += 1;
                }
                Value::Num(
                    core::str::from_utf8(&self.b[start..self.at])
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            }
        }
    }
}

fn vectors() -> Value {
    let text = include_str!("../../../../../../sdk/vectors/settings-vectors.json");
    Parser {
        b: text.as_bytes(),
        at: 0,
    }
    .value()
}

// ---- the shared vectors ---------------------------------------------------------------------------------------------------

#[test]
fn the_limits_are_the_contract() {
    let v = vectors();
    assert_eq!(v["area_bytes"].as_u64(), Some(AREA as u64));
    assert_eq!(v["key_max"].as_u64(), Some(KEY_MAX as u64));
    assert_eq!(v["value_max"].as_u64(), Some(VALUE_MAX as u64));
    assert_eq!(crate::settings::SECTORS, 16);
    assert_eq!(g::COPIES, 2);
    assert_eq!(kind_of(b"fx/route/core/fx"), Some("route"));
    assert_eq!(kind_of(b"fx/send/core/fx"), Some("u8x16"));
    assert_eq!(kind_of(b"fx/send/"), None);
    assert_eq!(kind_of(b"other"), None);
}

#[test]
fn valid_vectors_decode_and_encode_back() {
    let v = vectors();
    for c in v["valid"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let bytes = hex(c["hex"].as_str().unwrap());
        let want: Vec<(Vec<u8>, Vec<u8>)> = c["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e[0].as_str().unwrap().as_bytes().to_vec(),
                    hex(e[1].as_str().unwrap()),
                )
            })
            .collect();
        // the bytes as they are, and as a copy on the card (zero after them)
        let (seq, u) = check(&bytes).unwrap_or_else(|| panic!("{name}: refused"));
        assert_eq!(why(&bytes), "ok", "{name}");
        assert_eq!(
            (seq as u64, u as u64),
            (c["seq"].as_u64().unwrap(), c["used"].as_u64().unwrap()),
            "{name}"
        );
        let a = area_of(&bytes);
        assert_eq!(check(&a.0), Some((seq, u)), "{name}: padded");
        let got: Vec<(Vec<u8>, Vec<u8>)> = iter(&a.0, u)
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
        assert_eq!(got, want, "{name}");
        // the encoder gives the record's own bytes
        let mut e = new_record(0);
        for (k, val) in &want {
            assert_eq!(put(&mut e.0, k, val), Ok(true), "{name}");
        }
        seal(&mut e.0, seq);
        assert_eq!(&e.0[..u], &bytes[..u], "{name}: encode");
    }
}

#[test]
fn invalid_vectors_are_refused() {
    let v = vectors();
    for c in v["invalid"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let bytes = hex(c["hex"].as_str().unwrap());
        assert_eq!(check(&bytes), None, "{name}: exact bytes");
        assert_eq!(why(&bytes), c["why"].as_str().unwrap(), "{name}: why");
        assert_eq!(check(&area_of(&bytes).0), None, "{name}: on a card");
    }
}

// ---- edits ----------------------------------------------------------------------------------------------------------------

#[test]
fn put_get_replace_remove() {
    let mut a = new_record(1);
    assert_eq!(used(&a.0), HEADER);
    assert_eq!(put(&mut a.0, b"fx/route/m", &[1]), Ok(true));
    assert_eq!(put(&mut a.0, b"fx/send/m", &[7; 16]), Ok(true));
    assert_eq!(put(&mut a.0, b"zz", b"tail"), Ok(true));
    let mut out = [0u8; 32];
    assert_eq!(get(&a.0, b"fx/route/m", &mut out), 1);
    assert_eq!(out[0], 1);
    assert_eq!(get(&a.0, b"fx/send/m", &mut out), 16);
    assert_eq!(get(&a.0, b"nothing", &mut out), E_SET_NOKEY);
    // the same value: no change
    assert_eq!(put(&mut a.0, b"fx/route/m", &[1]), Ok(false));
    // a longer value moves the entries behind it, a shorter one moves them back
    assert_eq!(put(&mut a.0, b"fx/route/m", &[1, 2, 3, 4, 5]), Ok(true));
    assert_eq!(get(&a.0, b"fx/route/m", &mut out), 5);
    assert_eq!(&out[..5], &[1, 2, 3, 4, 5]);
    assert_eq!(get(&a.0, b"zz", &mut out), 4);
    assert_eq!(&out[..4], b"tail");
    assert_eq!(put(&mut a.0, b"fx/route/m", &[]), Ok(true));
    assert_eq!(get(&a.0, b"fx/route/m", &mut out), 0);
    assert_eq!(get(&a.0, b"fx/send/m", &mut out), 16);
    assert_eq!(out[..16], [7; 16]);
    // a small buffer
    let mut small = [0u8; 4];
    assert_eq!(get(&a.0, b"fx/send/m", &mut small), E_SET_VALUE);
    // remove: the middle one, then one that is not there
    assert_eq!(remove(&mut a.0, b"fx/send/m"), Ok(true));
    assert_eq!(remove(&mut a.0, b"fx/send/m"), Ok(false));
    assert_eq!(get(&a.0, b"fx/send/m", &mut out), E_SET_NOKEY);
    assert_eq!(get(&a.0, b"zz", &mut out), 4);
    seal(&mut a.0, 9);
    let u = used(&a.0);
    assert_eq!(check(&a.0).map(|c| c.1), Some(u));
    assert_eq!(entries(&a).len(), 2);
    assert_eq!(remove(&mut a.0, b"zz"), Ok(true));
    assert_eq!(remove(&mut a.0, b"fx/route/m"), Ok(true));
    assert_eq!(used(&a.0), HEADER);
}

#[test]
fn key_and_value_bounds() {
    let mut a = new_record(1);
    assert_eq!(put(&mut a.0, b"", b"x"), Err(E_SET_KEY));
    assert_eq!(put(&mut a.0, &[b'k'; KEY_MAX + 1], b"x"), Err(E_SET_KEY));
    assert_eq!(put(&mut a.0, &[b'k'; KEY_MAX], b"x"), Ok(true));
    assert_eq!(put(&mut a.0, b"has space", b"x"), Err(E_SET_KEY));
    assert_eq!(put(&mut a.0, b"tab\t", b"x"), Err(E_SET_KEY));
    assert_eq!(put(&mut a.0, &[b'a', 0xe4], b"x"), Err(E_SET_KEY));
    assert_eq!(
        put(&mut a.0, b"v", &std::vec![0u8; VALUE_MAX + 1]),
        Err(E_SET_VALUE)
    );
    assert_eq!(put(&mut a.0, b"v", &std::vec![9u8; VALUE_MAX]), Ok(true));
    let mut out = [0u8; 8];
    assert_eq!(get(&a.0, b"", &mut out), E_SET_KEY);
    assert_eq!(remove(&mut a.0, b"a b"), Err(E_SET_KEY));
}

#[test]
fn a_full_area_is_refused_and_left_as_it_was() {
    let mut a = new_record(1);
    let v = std::vec![5u8; VALUE_MAX];
    let mut n = 0;
    loop {
        let key = std::format!("k{n:03}");
        match put(&mut a.0, key.as_bytes(), &v) {
            Ok(true) => n += 1,
            Err(e) => {
                assert_eq!(e, E_SET_FULL);
                break;
            }
            Ok(false) => panic!("the key is new"),
        }
        assert!(n < 20);
    }
    // 8192 bytes: 18 of header and 1 + 4 + 2 + 1024 for each entry: 7 fit
    assert_eq!(n, 7);
    let before = a.0[..used(&a.0)].to_vec();
    assert_eq!(
        put(&mut a.0, b"another", b"x"),
        Ok(true),
        "a small entry still fits in the rest"
    );
    assert_eq!(remove(&mut a.0, b"another"), Ok(true));
    assert_eq!(&a.0[..used(&a.0)], &before[..]);
    // a replacement that would not fit is refused, the old value stays
    let mut a2 = new_record(1);
    let half = std::vec![1u8; 1024];
    for i in 0..7 {
        put(&mut a2.0, std::format!("k{i:03}").as_bytes(), &half).unwrap();
    }
    put(&mut a2.0, b"s", b"1").unwrap();
    let used0 = used(&a2.0);
    let room = AREA - used0;
    assert!(room < 1024);
    assert_eq!(
        put(&mut a2.0, b"s", &std::vec![2u8; 1 + room + 1]),
        Err(E_SET_FULL)
    );
    assert_eq!(used(&a2.0), used0);
    let mut out = [0u8; 4];
    assert_eq!(get(&a2.0, b"s", &mut out), 1);
    assert_eq!(out[0], b'1');
    // and one that fits to the byte is taken
    assert_eq!(put(&mut a2.0, b"s", &std::vec![2u8; 1 + room]), Ok(true));
    assert_eq!(used(&a2.0), AREA);
}

// ---- the two copies on the card -------------------------------------------------------------------------------------------

/// A record of three sectors: two entries of 700 bytes, all `fill`.
fn big(seq: u32, fill: u8) -> Box<Area> {
    let mut a = new_record(0);
    put(&mut a.0, b"k1", &[fill; 700]).unwrap();
    put(&mut a.0, b"k2", &[fill; 700]).unwrap();
    seal(&mut a.0, seq);
    assert_eq!(used(&a.0), 18 + 2 * (1 + 2 + 2 + 700));
    a
}

fn write_copy(copy: u32, seq: u32, key: &[u8], value: &[u8]) {
    let mut a = new_record(0);
    put(&mut a.0, key, value).unwrap();
    seal(&mut a.0, seq);
    assert_eq!(unsafe { write(&IO, &LAY, copy, &a.0) }, OK);
}

#[test]
fn the_geometry_is_the_documented_one() {
    assert_eq!(LAY.set_a() + 16, LAY.set_b());
    assert_eq!(LAY.set_b() + 16, LAY.sel_a());
    assert!(LAY.set_a() > LAY.first + LAY.header_b());
    assert_eq!(LAY.header_b() + 1 + 32 + 2, LAY.sectors);
}

#[test]
fn no_record_is_an_empty_one() {
    let _g = lock();
    fresh();
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!(
        (r.copy, r.seq, r.used, r.damaged),
        (NONE, 0, HEADER as u32, 0)
    );
    assert_eq!(used(&a.0), HEADER);
    assert_eq!(entries(&a).len(), 0);
}

#[test]
fn the_higher_seq_wins_and_a_equal_pair_takes_a() {
    let _g = lock();
    fresh();
    write_copy(0, 4, b"k", b"a4");
    write_copy(1, 5, b"k", b"b5");
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!((r.copy, r.seq, r.damaged), (1, 5, 0));
    assert_eq!(entries(&a), std::vec![(b"k".to_vec(), b"b5".to_vec())]);
    write_copy(0, 6, b"k", b"a6");
    let (r, a) = load_now();
    assert_eq!(
        (r.unwrap().copy, entries(&a)[0].1.clone()),
        (0, b"a6".to_vec())
    );
    write_copy(1, 6, b"k", b"b6");
    let (r, a) = load_now();
    assert_eq!(
        (r.unwrap().copy, entries(&a)[0].1.clone()),
        (0, b"a6".to_vec()),
        "equal: A"
    );
}

#[test]
fn a_damaged_newer_copy_leaves_the_older_one() {
    let _g = lock();
    for corrupt in 0..3 {
        fresh();
        write_copy(0, 1, b"k", b"old");
        write_copy(1, 2, b"k", b"new");
        let at = (LAY.set_b() - FIRST) as usize * 512;
        {
            let mut c = CARD.lock().unwrap();
            if corrupt == 0 {
                c[at + 20] ^= 0x40; // an entry byte: the crc fails
            } else if corrupt == 1 {
                c[at + 14] ^= 1; // the crc itself
            } else {
                c[at + 10 + 3] = 0xff; // used far over the copy's bytes
            }
        }
        let (r, a) = load_now();
        let r = r.unwrap();
        assert_eq!((r.copy, r.seq), (0, 1), "case {corrupt}");
        assert_eq!(r.damaged, 1, "case {corrupt}: reported damaged");
        assert_eq!(entries(&a), std::vec![(b"k".to_vec(), b"old".to_vec())]);
    }
}

#[test]
fn both_copies_damaged_is_an_empty_record() {
    let _g = lock();
    fresh();
    write_copy(0, 1, b"k", b"old");
    write_copy(1, 2, b"k", b"new");
    {
        let mut c = CARD.lock().unwrap();
        for s in [LAY.set_a(), LAY.set_b()] {
            c[(s - FIRST) as usize * 512 + 20] ^= 1;
        }
    }
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!((r.copy, r.damaged), (NONE, 2));
    assert_eq!(entries(&a).len(), 0);
}

#[test]
fn a_power_cut_in_a_write_leaves_the_current_copy() {
    let _g = lock();
    fresh();
    // the current record is copy A (seq 1, three sectors), the write goes to B
    assert_eq!(unsafe { write(&IO, &LAY, 0, &big(1, 1).0) }, OK);
    let b = big(2, 2);
    // cut after one of its sectors: the first sector of the new record, the others still zero
    {
        let mut c = CARD.lock().unwrap();
        let at = (LAY.set_b() - FIRST) as usize * 512;
        c[at..at + 512].copy_from_slice(&b.0[..512]);
    }
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!((r.copy, r.seq, r.damaged), (0, 1, 1));
    assert_eq!(entries(&a)[0].1, std::vec![1u8; 700]);
    // the write completed
    assert_eq!(unsafe { write(&IO, &LAY, 1, &b.0) }, OK);
    let (r, a) = load_now();
    assert_eq!((r.unwrap().copy, entries(&a)[0].1[0]), (1, 2));
}

#[test]
fn a_module_key_is_its_name_a_slash_and_the_key() {
    let mut out = [0u8; KEY_MAX];
    let n = module_key(b"test/m", b"volume", &mut out).unwrap();
    assert_eq!(&out[..n], b"test/m/volume");
    let n = module_key(b"test/m", b"a.b-c", &mut out).unwrap();
    assert_eq!(&out[..n], b"test/m/a.b-c");
    // the longest key that fits: 47 characters in all
    let key = [b'k'; KEY_MAX - 7];
    assert_eq!(module_key(b"test/m", &key, &mut out), Some(KEY_MAX));
}

#[test]
fn a_module_key_that_would_leave_its_prefix_or_break_the_rules_is_refused() {
    let mut out = [0u8; KEY_MAX];
    assert_eq!(module_key(b"test/m", b"", &mut out), None, "empty key");
    assert_eq!(module_key(b"", b"k", &mut out), None, "empty module name");
    assert_eq!(
        module_key(b"test/m", b"/other/k", &mut out),
        None,
        "leading slash"
    );
    assert_eq!(
        module_key(b"test", b"m/x", &mut out),
        None,
        "slash inside: would collide with test/m + x"
    );
    assert_eq!(module_key(b"test/m", b"../other/k", &mut out), None, "..");
    assert_eq!(
        module_key(b"test/m", b"a/../b", &mut out),
        None,
        ".. inside"
    );
    assert_eq!(
        module_key(b"test/m", b"a..", &mut out),
        None,
        ".. at the end"
    );
    assert_eq!(module_key(b"test/m", b"has space", &mut out), None, "space");
    assert_eq!(
        module_key(b"test/m", b"tab\t", &mut out),
        None,
        "control byte"
    );
    assert_eq!(
        module_key(b"test/m", b"caf\xc3\xa9", &mut out),
        None,
        "not ASCII"
    );
    let key = [b'k'; KEY_MAX - 6];
    assert_eq!(
        module_key(b"test/m", &key, &mut out),
        None,
        "48 characters in all"
    );
}

#[test]
fn a_write_takes_only_the_sectors_the_record_needs_and_only_its_copy() {
    let _g = lock();
    fresh();
    write_copy(1, 3, b"k", b"v");
    assert_eq!(*WRITES.lock().unwrap(), std::vec![(LAY.set_b(), 1)]);
    WRITES.lock().unwrap().clear();
    let a = big(4, 3);
    assert_eq!(unsafe { write(&IO, &LAY, 0, &a.0) }, OK);
    assert_eq!(*WRITES.lock().unwrap(), std::vec![(LAY.set_a(), 3)]);
    // a failing card write is `E_WRITE`
    *FAIL.lock().unwrap() = (u32::MAX, 0, 0, 0);
    assert_eq!(unsafe { write(&IO, &LAY, 0, &a.0) }, E_WRITE);
}

#[test]
fn a_shorter_record_over_a_longer_one_ignores_the_old_tail() {
    let _g = lock();
    fresh();
    assert_eq!(unsafe { write(&IO, &LAY, 0, &big(1, 7).0) }, OK);
    write_copy(0, 2, b"k", b"s"); // the same copy, shorter: sectors 1 and 2 still hold the old bytes
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!((r.copy, r.seq, r.damaged), (0, 2, 0));
    assert_eq!(entries(&a), std::vec![(b"k".to_vec(), b"s".to_vec())]);
}

#[test]
fn a_read_error_is_reported_and_leaves_an_empty_record() {
    let _g = lock();
    fresh();
    write_copy(0, 1, b"k", b"v");
    *FAIL.lock().unwrap() = (0, u32::MAX, 0, 0);
    let (r, a) = load_now();
    assert_eq!(r.err(), Some(E_READ));
    assert_eq!(used(&a.0), HEADER);
    // only the full read of the chosen copy fails: the other copy is tried
    fresh();
    write_copy(0, 1, b"k", b"old");
    write_copy(1, 2, b"k", b"new");
    *FAIL_ONE.lock().unwrap() = Some(2); // both first sectors read (reads 0 and 1), the full read of B (read 2) fails
    let (r, a) = load_now();
    let r = r.unwrap();
    assert_eq!((r.copy, r.seq), (0, 1));
    assert_eq!(entries(&a)[0].1, b"old".to_vec());
}
