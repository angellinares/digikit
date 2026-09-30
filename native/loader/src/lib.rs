//! Portable decoder for Elektron ELE3 firmware transported in MIDI SysEx.
//!
//! This crate deliberately does no file or browser I/O. Call [`parse`] with a
//! complete `.syx` byte slice and use the returned decoded sections.

use std::fmt;

const ELE3: &[u8; 4] = b"ELE3";
const COUNT_OFF: usize = 0x1c;
const TABLE_OFF: usize = 0x20;
const ENTRY_SIZE: usize = 16;
const ELZ_BIAS: u32 = 767;
const ELZ_REUSE: u32 = 2;
const ELZ_FAR: u32 = 3328;
/// Prevents malformed compressed data from requesting an unbounded allocation.
pub const DEFAULT_MAX_DEPACKED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub container_offset: usize,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: u32,
    /// Offset within the ELE3 container (the offset convention used by its table).
    pub container_offset: u32,
    pub stored_length: u32,
    pub destination: u32,
    pub encoding: SectionEncoding,
    /// The decoded payload. For raw sections this excludes a raw 8-byte header.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEncoding {
    Elz { stream_length: u32, byte_sum: u32 },
    RawWithHeader,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    MissingSysExStart,
    UnterminatedSysEx,
    MissingContainer,
    TruncatedContainer {
        needed: usize,
        available: usize,
    },
    InvalidSectionRange {
        offset: u32,
        length: u32,
        container_length: usize,
    },
    TruncatedElzHeader,
    ElzEndMarker {
        declared_length: u32,
        consumed: usize,
    },
    ElzUnexpectedEof,
    ElzGammaOverflow,
    ElzInvalidBackReference {
        offset: u32,
        output_length: usize,
    },
    ElzOutputLimit {
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.kind, self.offset)
    }
}
impl std::error::Error for Error {}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSysExStart => f.write_str("expected SysEx F0"),
            Self::UnterminatedSysEx => f.write_str("unterminated SysEx message"),
            Self::MissingContainer => f.write_str("no ELE3 container"),
            Self::TruncatedContainer { needed, available } => write!(
                f,
                "truncated ELE3 container (need {needed}, have {available})"
            ),
            Self::InvalidSectionRange {
                offset,
                length,
                container_length,
            } => write!(
                f,
                "section range {offset:#x}+{length:#x} is outside {container_length:#x}-byte container"
            ),
            Self::TruncatedElzHeader => f.write_str("truncated ELZ stream header"),
            Self::ElzEndMarker {
                declared_length,
                consumed,
            } => write!(
                f,
                "ELZ end marker consumed {consumed} of declared {declared_length} bytes"
            ),
            Self::ElzUnexpectedEof => f.write_str("unexpected end of ELZ stream"),
            Self::ElzGammaOverflow => f.write_str("ELZ gamma value overflow"),
            Self::ElzInvalidBackReference {
                offset,
                output_length,
            } => write!(
                f,
                "ELZ back-reference {offset} outside {output_length} output bytes"
            ),
            Self::ElzOutputLimit { limit } => write!(f, "ELZ output exceeds {limit}-byte limit"),
        }
    }
}

/// Decodes SysEx8-in-7 transport, parses ELE3, and decodes all packed sections.
pub fn parse(syx: &[u8]) -> Result<Firmware, Error> {
    parse_with_limit(syx, DEFAULT_MAX_DEPACKED_BYTES)
}

/// As [`parse`], with a caller-selected maximum decoded size per ELZ section.
pub fn parse_with_limit(syx: &[u8], max_depacked_bytes: usize) -> Result<Firmware, Error> {
    let decoded = decode_syx(syx)?;
    let Some(container_offset) = decoded.windows(ELE3.len()).position(|w| w == ELE3) else {
        return Err(err(0, ErrorKind::MissingContainer));
    };
    parse_container(
        &decoded[container_offset..],
        container_offset,
        max_depacked_bytes,
    )
}

/// Decodes the SysEx 8-in-7 transport used by the existing Python loader.
pub fn decode_syx(syx: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < syx.len() {
        if syx[at] != 0xf0 {
            return Err(err(at, ErrorKind::MissingSysExStart));
        }
        let Some(end_rel) = syx[at + 1..].iter().position(|&b| b == 0xf7) else {
            return Err(err(at, ErrorKind::UnterminatedSysEx));
        };
        let end = at + 1 + end_rel;
        let body = &syx[at + 1..end];
        at = end + 1;
        // Python intentionally ignores non-firmware messages and does not inspect headers.
        if body.len() != 126 {
            continue;
        }
        let payload = &body[9..125];
        let mut p = 0;
        while p < payload.len() {
            let high_bits = payload[p];
            p += 1;
            for bit in 0..7 {
                if p == payload.len() {
                    break;
                }
                out.push(
                    payload[p]
                        | if high_bits & (1 << (6 - bit)) != 0 {
                            0x80
                        } else {
                            0
                        },
                );
                p += 1;
            }
        }
    }
    Ok(out)
}

fn parse_container(c: &[u8], container_offset: usize, max: usize) -> Result<Firmware, Error> {
    need(c, COUNT_OFF + 4, container_offset)?;
    let count = be_u32(c, COUNT_OFF) as usize;
    let table_end = TABLE_OFF
        .checked_add(count.checked_mul(ENTRY_SIZE).ok_or_else(|| {
            err(
                container_offset + COUNT_OFF,
                ErrorKind::TruncatedContainer {
                    needed: usize::MAX,
                    available: c.len(),
                },
            )
        })?)
        .ok_or_else(|| {
            err(
                container_offset + TABLE_OFF,
                ErrorKind::TruncatedContainer {
                    needed: usize::MAX,
                    available: c.len(),
                },
            )
        })?;
    need(c, table_end, container_offset)?;
    let mut sections = Vec::new();
    sections.try_reserve(count).map_err(|_| {
        err(
            container_offset + TABLE_OFF,
            ErrorKind::TruncatedContainer {
                needed: count,
                available: 0,
            },
        )
    })?;
    for index in 0..count {
        let entry = TABLE_OFF + index * ENTRY_SIZE;
        let id = be_u32(c, entry);
        let offset = be_u32(c, entry + 4);
        let stored_length = be_u32(c, entry + 8);
        let destination = be_u32(c, entry + 12);
        let start = usize::try_from(offset).map_err(|_| {
            err(
                container_offset + entry + 4,
                ErrorKind::InvalidSectionRange {
                    offset,
                    length: stored_length,
                    container_length: c.len(),
                },
            )
        })?;
        let end = start.checked_add(stored_length as usize).ok_or_else(|| {
            err(
                container_offset + entry + 4,
                ErrorKind::InvalidSectionRange {
                    offset,
                    length: stored_length,
                    container_length: c.len(),
                },
            )
        })?;
        if end > c.len() {
            return Err(err(
                container_offset + entry + 4,
                ErrorKind::InvalidSectionRange {
                    offset,
                    length: stored_length,
                    container_length: c.len(),
                },
            ));
        }
        let stream = &c[start..end];
        let (encoding, bytes) = classify_and_decode(stream, container_offset + start, max)?;
        sections.push(Section {
            id,
            container_offset: offset,
            stored_length,
            destination,
            encoding,
            bytes,
        });
    }
    Ok(Firmware {
        container_offset,
        sections,
    })
}

fn classify_and_decode(
    stream: &[u8],
    absolute_offset: usize,
    max: usize,
) -> Result<(SectionEncoding, Vec<u8>), Error> {
    if stream.len() >= 8 {
        let length = be_u32(stream, 0);
        let byte_sum = be_u32(stream, 4);
        let payload_end = 8usize.checked_add(length as usize);
        if let Some(end) = payload_end.filter(|&end| end <= stream.len()) {
            let actual = stream[8..end]
                .iter()
                .fold(0u32, |sum, &b| sum.wrapping_add(b as u32));
            if actual == byte_sum {
                let bytes = depack_section(stream, absolute_offset, max)?;
                return Ok((
                    SectionEncoding::Elz {
                        stream_length: length,
                        byte_sum,
                    },
                    bytes,
                ));
            }
        }
        if byte_sum == 0 {
            return Ok((SectionEncoding::RawWithHeader, stream[8..].to_vec()));
        }
    }
    Ok((SectionEncoding::Raw, stream.to_vec()))
}

/// Decodes a section stream including its `[u32 BE length][u32 BE byte sum]` header.
pub fn depack_section(stream: &[u8], absolute_offset: usize, max: usize) -> Result<Vec<u8>, Error> {
    if stream.len() < 8 {
        return Err(err(absolute_offset, ErrorKind::TruncatedElzHeader));
    }
    let length = be_u32(stream, 0) as usize;
    let end = 8usize
        .checked_add(length)
        .ok_or_else(|| err(absolute_offset, ErrorKind::ElzUnexpectedEof))?;
    if end > stream.len() {
        return Err(err(
            absolute_offset + stream.len(),
            ErrorKind::ElzUnexpectedEof,
        ));
    }
    let mut bits = Bits {
        data: &stream[8..end],
        pos: 0,
        tag: 0,
        base: absolute_offset + 8,
    };
    let mut out = Vec::new();
    let mut last = 1u32;
    loop {
        if bits.bit()? != 0 {
            push_byte(&mut out, bits.byte()?, max, bits.offset())?;
            continue;
        }
        let gamma = bits.gamma()?;
        let offset = if gamma == ELZ_REUSE {
            last
        } else {
            let raw = gamma.wrapping_shl(8).wrapping_add(bits.byte()? as u32);
            if raw == ELZ_BIAS {
                if bits.pos != length {
                    return Err(err(
                        bits.offset(),
                        ErrorKind::ElzEndMarker {
                            declared_length: length as u32,
                            consumed: bits.pos,
                        },
                    ));
                }
                return Ok(out);
            }
            let offset = raw.wrapping_sub(ELZ_BIAS);
            last = offset;
            offset
        };
        let short = 2 * bits.bit()? as u32 + bits.bit()? as u32;
        let mut length = if short == 0 {
            bits.gamma()?
                .checked_add(2)
                .ok_or_else(|| err(bits.offset(), ErrorKind::ElzGammaOverflow))?
        } else {
            short
        };
        if offset > ELZ_FAR {
            length = length
                .checked_add(1)
                .ok_or_else(|| err(bits.offset(), ErrorKind::ElzGammaOverflow))?;
        }
        if offset == 0 || offset as usize > out.len() {
            return Err(err(
                bits.offset(),
                ErrorKind::ElzInvalidBackReference {
                    offset,
                    output_length: out.len(),
                },
            ));
        }
        let copies = length
            .checked_add(1)
            .ok_or_else(|| err(bits.offset(), ErrorKind::ElzGammaOverflow))?;
        for _ in 0..copies {
            let b = out[out.len() - offset as usize];
            push_byte(&mut out, b, max, bits.offset())?;
        }
    }
}

fn push_byte(out: &mut Vec<u8>, b: u8, max: usize, offset: usize) -> Result<(), Error> {
    if out.len() >= max {
        return Err(err(offset, ErrorKind::ElzOutputLimit { limit: max }));
    }
    out.try_reserve(1)
        .map_err(|_| err(offset, ErrorKind::ElzOutputLimit { limit: max }))?;
    out.push(b);
    Ok(())
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    tag: u16,
    base: usize,
}
impl Bits<'_> {
    fn offset(&self) -> usize {
        self.base + self.pos
    }
    fn byte(&mut self) -> Result<u8, Error> {
        if self.pos == self.data.len() {
            return Err(err(self.offset(), ErrorKind::ElzUnexpectedEof));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }
    fn bit(&mut self) -> Result<u8, Error> {
        self.tag = (self.tag << 1) & 0x1ff;
        if self.tag & 0xff == 0 {
            let b = self.byte()?;
            self.tag = ((b as u16) << 1) | 1;
            Ok(b >> 7)
        } else {
            Ok((self.tag >> 8) as u8)
        }
    }
    fn gamma(&mut self) -> Result<u32, Error> {
        let mut v = 1u32;
        loop {
            v = v
                .checked_shl(1)
                .ok_or_else(|| err(self.offset(), ErrorKind::ElzGammaOverflow))?
                | self.bit()? as u32;
            if self.bit()? != 0 {
                return Ok(v);
            }
            if v > 0x0200_0000 {
                return Err(err(self.offset(), ErrorKind::ElzGammaOverflow));
            }
        }
    }
}

fn be_u32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().expect("checked by caller"))
}
fn need(data: &[u8], needed: usize, base: usize) -> Result<(), Error> {
    if data.len() < needed {
        Err(err(
            base + data.len(),
            ErrorKind::TruncatedContainer {
                needed,
                available: data.len(),
            },
        ))
    } else {
        Ok(())
    }
}
fn err(offset: usize, kind: ErrorKind) -> Error {
    Error { offset, kind }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_syx_and_container_table() {
        assert_eq!(
            decode_syx(&[0xf0]).unwrap_err().kind,
            ErrorKind::UnterminatedSysEx
        );
        let mut c = b"ELE3".to_vec();
        c.resize(COUNT_OFF + 4, 0);
        c[COUNT_OFF..COUNT_OFF + 4].copy_from_slice(&1u32.to_be_bytes());
        assert!(matches!(
            parse_container(&c, 0, 32).unwrap_err().kind,
            ErrorKind::TruncatedContainer { .. }
        ));
    }

    #[test]
    fn rejects_section_outside_container() {
        let mut c = b"ELE3".to_vec();
        c.resize(TABLE_OFF + ENTRY_SIZE, 0);
        c[COUNT_OFF..COUNT_OFF + 4].copy_from_slice(&1u32.to_be_bytes());
        c[TABLE_OFF + 4..TABLE_OFF + 8].copy_from_slice(&0x100u32.to_be_bytes());
        c[TABLE_OFF + 8..TABLE_OFF + 12].copy_from_slice(&1u32.to_be_bytes());
        assert!(matches!(
            parse_container(&c, 0, 32).unwrap_err().kind,
            ErrorKind::InvalidSectionRange { .. }
        ));
    }

    #[test]
    fn rejects_invalid_elz_back_reference() {
        // Control bits 0, 1, 1, 0, 1: match; gamma 3; short length 1.
        // Raw 0x300 gives offset 1, invalid before any output exists.
        let stream = [0, 0, 0, 2, 0, 0, 0, 0, 0b0110_1000, 0x00];
        assert!(matches!(
            depack_section(&stream, 0, 32).unwrap_err().kind,
            ErrorKind::ElzInvalidBackReference {
                offset: 1,
                output_length: 0
            }
        ));
    }

    #[test]
    fn parses_repo_fixtures_when_present() {
        for file in [
            "../../Digitakt_II_OS1.16.syx",
            "../../Digitone_II_OS1.11.syx",
        ] {
            let Ok(data) = std::fs::read(file) else {
                continue;
            };
            let firmware = parse(&data).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(
                firmware
                    .sections
                    .iter()
                    .any(|s| s.id == 2 && matches!(s.encoding, SectionEncoding::Elz { .. }))
            );
            assert!(
                firmware
                    .sections
                    .iter()
                    .any(|s| s.id == 4 && matches!(s.encoding, SectionEncoding::RawWithHeader))
            );
        }
    }
}
