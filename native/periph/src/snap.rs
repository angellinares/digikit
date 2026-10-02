//! Minimal binary state writer/reader for the opt-in machine snapshot.
//!
//! Little-endian primitives, length-prefixed byte strings and four-byte
//! section tags. Components write only mutable run state; configuration
//! comes from constructing the same machine again before `snap_load`.

pub type Result<T> = std::result::Result<T, String>;

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn tag(&mut self, tag: &str) {
        debug_assert_eq!(tag.len(), 4);
        self.buf.extend_from_slice(tag.as_bytes());
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    pub fn opt_f64(&mut self, v: Option<f64>) {
        self.bool(v.is_some());
        self.f64(v.unwrap_or(0.0));
    }
    pub fn opt_u64(&mut self, v: Option<u64>) {
        self.bool(v.is_some());
        self.u64(v.unwrap_or(0));
    }
    pub fn opt_u32(&mut self, v: Option<u32>) {
        self.bool(v.is_some());
        self.u32(v.unwrap_or(0));
    }
    /// Length-prefixed bytes.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
    /// Fixed-size bytes the reader knows the length of.
    pub fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn is_empty(&self) -> bool {
        self.pos == self.data.len()
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or("snapshot truncated")?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn tag(&mut self, tag: &str) -> Result<()> {
        let at = self.pos;
        if self.take(4)? != tag.as_bytes() {
            return Err(format!("snapshot section {tag:?} expected at byte {at}"));
        }
        Ok(())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("snapshot bool out of range".into()),
        }
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub fn opt_f64(&mut self) -> Result<Option<f64>> {
        let some = self.bool()?;
        let v = self.f64()?;
        Ok(some.then_some(v))
    }
    pub fn opt_u64(&mut self) -> Result<Option<u64>> {
        let some = self.bool()?;
        let v = self.u64()?;
        Ok(some.then_some(v))
    }
    pub fn opt_u32(&mut self) -> Result<Option<u32>> {
        let some = self.bool()?;
        let v = self.u32()?;
        Ok(some.then_some(v))
    }
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = usize::try_from(self.u64()?).map_err(|_| "snapshot length")?;
        self.take(n)
    }
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }
    /// A count that must be within a sane bound before allocating.
    pub fn len(&mut self, max: usize) -> Result<usize> {
        let n = usize::try_from(self.u64()?).map_err(|_| "snapshot length")?;
        if n > max {
            return Err(format!("snapshot count {n} exceeds {max}"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_round_trip() {
        let mut w = Writer::new();
        w.tag("TEST");
        w.u8(7);
        w.bool(true);
        w.u16(0xbeef);
        w.u32(0xdead_beef);
        w.u64(u64::MAX - 3);
        w.f64(1.5);
        w.opt_f64(None);
        w.opt_u64(Some(9));
        w.bytes(b"abc");
        let mut r = Reader::new(&w.buf);
        r.tag("TEST").unwrap();
        assert_eq!(r.u8().unwrap(), 7);
        assert!(r.bool().unwrap());
        assert_eq!(r.u16().unwrap(), 0xbeef);
        assert_eq!(r.u32().unwrap(), 0xdead_beef);
        assert_eq!(r.u64().unwrap(), u64::MAX - 3);
        assert_eq!(r.f64().unwrap(), 1.5);
        assert_eq!(r.opt_f64().unwrap(), None);
        assert_eq!(r.opt_u64().unwrap(), Some(9));
        assert_eq!(r.bytes().unwrap(), b"abc");
        assert!(r.is_empty());
        assert!(r.u8().is_err());
        assert!(Reader::new(b"XXXX").tag("TEST").is_err());
    }
}
