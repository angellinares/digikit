//! Reader for the `DT2MMIO` v1/v2 trace format `emu/mmiotrace.py` writes.
//!
//! Format (`emu/mmiotrace.py`'s module docstring is the full spec; this is
//! the subset this crate needs):
//!
//! ```text
//! magic    8 bytes   b"DT2MMIO\0"
//! version  u16       1 or 2 (v2 adds `SR` boundary samples)
//! flags    u16       bit 0: body is a zlib (RFC 1950) stream
//! hdr_len  u32
//! header   hdr_len bytes of UTF-8 JSON
//! body     records until EOF
//! ```
//!
//! Every record is a `u8` tag then a fixed little-endian layout, some with
//! trailing bytes whose length is the layout's last field. A `TIME` record
//! sets the clock for every record after it (this reader tracks it and
//! attaches it to each [`Record`], the same convenience `emu.mmiotrace.Reader`
//! gives Python callers). An unrecognised tag is a hard error: a record's
//! length is determined by its tag, so nothing can be skipped past it.

use std::fs::File;
use std::io::{self, BufReader, Read};

use flate2::read::ZlibDecoder;

const MAGIC: &[u8; 8] = b"DT2MMIO\0";
const VERSION: u16 = 2;
const OLDEST_VERSION: u16 = 1;
const FLAG_ZLIB: u16 = 1;

pub const TIME: u8 = 0x01;
pub const STEP: u8 = 0x02;
pub const RD: u8 = 0x03;
pub const WR: u8 = 0x04;
pub const IRQ: u8 = 0x05;
pub const RTE: u8 = 0x06;
pub const HWR: u8 = 0x07;
pub const HRD: u8 = 0x08;
pub const HREG: u8 = 0x09;
pub const SRC: u8 = 0x0a;
pub const STATE: u8 = 0x0b;
pub const PAGE: u8 = 0x0c;
pub const MARK: u8 = 0x0d;
pub const RATE: u8 = 0x0e;
pub const END: u8 = 0x0f;
/// v2: a live guest SR sample immediately before a service boundary.
pub const SR: u8 = 0x10;

pub const IRQ_TAKEN: u8 = 1;
pub const IRQ_SYNC: u8 = 2;

#[derive(Debug)]
pub enum TraceError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u16),
    Json(String),
    Truncated(&'static str),
    UnknownTag(u8),
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::Io(e) => write!(f, "io error: {e}"),
            TraceError::BadMagic => write!(f, "not a DT2MMIO trace"),
            TraceError::UnsupportedVersion(v) => write!(f, "trace version {v}, reader {VERSION}"),
            TraceError::Json(e) => write!(f, "header JSON: {e}"),
            TraceError::Truncated(what) => write!(f, "truncated {what}"),
            TraceError::UnknownTag(t) => write!(f, "unknown record tag {t:#x}"),
        }
    }
}
impl std::error::Error for TraceError {}
impl From<io::Error> for TraceError {
    fn from(e: io::Error) -> Self {
        TraceError::Io(e)
    }
}

/// One decoded record. `fields` holds the fixed layout as u64s (widened;
/// callers cast down per-field, matching the widths the format table
/// documents); `data` holds any trailing bytes.
#[derive(Debug, Clone)]
pub struct Record {
    pub tag: u8,
    pub clock: u64,
    pub fields: Vec<u64>,
    pub data: Vec<u8>,
}

impl Record {
    pub fn u32(&self, i: usize) -> u32 {
        self.fields[i] as u32
    }
    pub fn u16(&self, i: usize) -> u16 {
        self.fields[i] as u16
    }
    pub fn u8(&self, i: usize) -> u8 {
        self.fields[i] as u8
    }
}

pub struct Reader {
    pub header: serde_json::Value,
    /// On-disk protocol version; v1 traces simply contain no [`SR`] records.
    pub version: u16,
    body: Box<dyn Read>,
    buf: Vec<u8>,
    pos: usize,
    clock: u64,
    pub sources: std::collections::HashMap<u16, String>,
}

impl Reader {
    pub fn open(path: &str) -> Result<Self, TraceError> {
        let mut f = BufReader::new(File::open(path)?);
        let mut pre = [0u8; 16];
        f.read_exact(&mut pre)
            .map_err(|_| TraceError::Truncated("preamble"))?;
        if &pre[0..8] != MAGIC {
            return Err(TraceError::BadMagic);
        }
        let version = u16::from_le_bytes([pre[8], pre[9]]);
        if !(OLDEST_VERSION..=VERSION).contains(&version) {
            return Err(TraceError::UnsupportedVersion(version));
        }
        let flags = u16::from_le_bytes([pre[10], pre[11]]);
        let hdr_len = u32::from_le_bytes([pre[12], pre[13], pre[14], pre[15]]) as usize;
        let mut hdr_buf = vec![0u8; hdr_len];
        f.read_exact(&mut hdr_buf)
            .map_err(|_| TraceError::Truncated("header"))?;
        let header: serde_json::Value =
            serde_json::from_slice(&hdr_buf).map_err(|e| TraceError::Json(e.to_string()))?;
        let body: Box<dyn Read> = if flags & FLAG_ZLIB != 0 {
            Box::new(ZlibDecoder::new(f))
        } else {
            Box::new(f)
        };
        Ok(Self {
            header,
            version,
            body,
            buf: Vec::with_capacity(1 << 20),
            pos: 0,
            clock: 0,
            sources: std::collections::HashMap::new(),
        })
    }

    fn need(&mut self, n: usize) -> Result<bool, TraceError> {
        while self.buf.len() - self.pos < n {
            if self.pos > 0 {
                self.buf.drain(0..self.pos);
                self.pos = 0;
            }
            let mut chunk = [0u8; 1 << 16];
            let got = self.body.read(&mut chunk)?;
            if got == 0 {
                return Ok(false);
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        Ok(true)
    }

    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        s
    }

    fn u16le(b: &[u8]) -> u64 {
        u16::from_le_bytes([b[0], b[1]]) as u64
    }
    fn u32le(b: &[u8]) -> u64 {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64
    }
    fn u64le(b: &[u8]) -> u64 {
        u64::from_le_bytes(b.try_into().unwrap())
    }
    fn u8v(b: &[u8]) -> u64 {
        b[0] as u64
    }

    /// -> the next record, or None at a clean EOF.
    pub fn next_record(&mut self) -> Result<Option<Record>, TraceError> {
        if !self.need(1)? {
            return Ok(None);
        }
        let tag = self.take(1)[0];
        // (fixed-size fields as byte-widths, trailing?)
        let (widths, trailing): (&[usize], bool) = match tag {
            TIME => (&[8], false),
            STEP => (&[4, 4], false),
            RD | WR => (&[4, 4, 4, 1], false),
            IRQ => (&[2, 1, 1, 2, 4, 4, 2], false),
            RTE => (&[4, 2], false),
            HWR | HRD => (&[2, 4, 4], true),
            HREG => (&[2, 2, 4], false),
            SRC => (&[2, 2], true),
            STATE => (&[4], true),
            PAGE => (&[4, 4], true),
            MARK => (&[4], true),
            RATE => (&[8], false),
            END => (&[4], true),
            SR => (&[2], false),
            _ => return Err(TraceError::UnknownTag(tag)),
        };
        let fixed_len: usize = widths.iter().sum();
        if !self.need(fixed_len)? {
            return Err(TraceError::Truncated("fixed fields"));
        }
        let raw = self.take(fixed_len).to_vec();
        let mut fields = Vec::with_capacity(widths.len());
        let mut off = 0;
        for &w in widths {
            let b = &raw[off..off + w];
            fields.push(match w {
                1 => Self::u8v(b),
                2 => Self::u16le(b),
                4 => Self::u32le(b),
                8 => Self::u64le(b),
                _ => unreachable!(),
            });
            off += w;
        }
        let mut data = Vec::new();
        if trailing {
            let n = *fields.last().unwrap() as usize;
            if !self.need(n)? {
                return Err(TraceError::Truncated("trailing data"));
            }
            data = self.take(n).to_vec();
        }
        if tag == TIME {
            self.clock = fields[0];
        } else if tag == SRC {
            let name = String::from_utf8_lossy(&data).to_string();
            self.sources.insert(fields[0] as u16, name);
        }
        Ok(Some(Record {
            tag,
            clock: self.clock,
            fields,
            data,
        }))
    }
}

impl Iterator for Reader {
    type Item = Result<Record, TraceError>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_record() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(version: u16, body: &[u8]) -> std::path::PathBuf {
        let mut bytes = Vec::new();
        let header = br#"{"format":"dt2-mmio-trace"}"#;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(body);
        let path = std::env::temp_dir().join(format!(
            "periph-trace-{}-{}.mmio",
            std::process::id(),
            version
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reads_v1_without_boundary_samples() {
        let mut body = vec![TIME];
        body.extend_from_slice(&7u64.to_le_bytes());
        let path = trace(1, &body);
        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reader.version, 1);
        let record = reader.next_record().unwrap().unwrap();
        assert_eq!(
            (record.tag, record.clock, record.fields),
            (TIME, 7, vec![7])
        );
        assert!(reader.next_record().unwrap().is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_v2_boundary_sample_in_stream_order() {
        let mut body = vec![TIME];
        body.extend_from_slice(&9u64.to_le_bytes());
        body.push(SR);
        body.extend_from_slice(&0x2004u16.to_le_bytes());
        body.push(IRQ);
        body.extend_from_slice(&207u16.to_le_bytes());
        body.extend_from_slice(&[3, IRQ_TAKEN]);
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&[0; 10]);
        let path = trace(2, &body);
        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reader.version, 2);
        let records: Vec<_> = reader.by_ref().map(Result::unwrap).collect();
        assert_eq!(
            records.iter().map(|r| r.tag).collect::<Vec<_>>(),
            [TIME, SR, IRQ]
        );
        assert_eq!(records[1].fields, vec![0x2004]);
        assert_eq!(records[1].clock, records[2].clock);
        std::fs::remove_file(path).unwrap();
    }
}
