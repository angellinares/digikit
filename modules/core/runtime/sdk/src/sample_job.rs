//! Immutable sample publication. Failed transfers retain their source until the link is terminal.
/// Validate the version-one little-endian bank record and its initially empty descriptor.
pub fn valid_bank(record: &[u8], image: &[u8], bss: u32) -> bool {
    if record.len() != 32 || image.len() > u32::MAX as usize {
        return false;
    }
    let word = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().unwrap());
    let (start, frames, desc) = (word(8), word(12), word(16));
    let size = image.len() as u32;
    word(0) == 1
        && (1..=8192).contains(&frames)
        && start & 3 == 0
        && desc & 3 == 0
        && start >= size
        && start - size <= bss
        && frames * 4 <= bss - (start - size)
        && desc <= size
        && 16 <= size - desc
        && image[desc as usize..desc as usize + 16]
            .iter()
            .all(|x| *x == 0)
        && word(20) == 0
        && word(24) == 0
        && word(28) == 0
}

/// Validate the version-one sample-table record, kind 2 (little-endian words: kind, local id, records and control byte offsets,
/// four zero words): 16 records of 512 bytes and 16 control sets of 256 bytes, in the initialized image, zero, not overlapping.
pub fn valid_tracks(record: &[u8], image: &[u8]) -> bool {
    if record.len() != 32 || image.len() > u32::MAX as usize {
        return false;
    }
    let word = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().unwrap());
    let (records, control) = (word(8), word(12));
    let inside = |at: u32, len: u32| {
        at & 3 == 0
            && (at as usize).checked_add(len as usize).is_some_and(|e| e <= image.len())
            && image[at as usize..(at + len) as usize].iter().all(|x| *x == 0)
    };
    word(0) == 2
        && inside(records, TRACK_RECORDS_BYTES)
        && inside(control, TRACK_CONTROL_BYTES)
        && (records >= control + TRACK_CONTROL_BYTES || control >= records + TRACK_RECORDS_BYTES)
        && word(16) == 0
        && word(20) == 0
        && word(24) == 0
        && word(28) == 0
}
pub const TRACK_RECORDS_BYTES: u32 = 512;
pub const TRACK_CONTROL_BYTES: u32 = 256;

pub trait Link {
    /// 0 accepted, -2 invalid, other values busy.
    fn send(&mut self, destination: u32, source: *const u8, bytes: u32) -> i32;
    /// None means the link can still dereference its source.
    fn poll(&mut self) -> Option<bool>;
}

pub struct Job {
    pub status: i32,
    pub source: *mut u8,
    pub frames: u32,
    destination: u32,
    descriptor: u32,
    phase: u8,
    started: bool,
    ticks: u32,
    fields: [u8; 12],
    ready: [u8; 4],
}
impl Job {
    pub const fn new() -> Self {
        Self {
            status: 0,
            source: core::ptr::null_mut(),
            frames: 0,
            destination: 0,
            descriptor: 0,
            phase: 0,
            started: false,
            ticks: 0,
            fields: [0; 12],
            ready: [1, 0, 0, 0],
        }
    }
    pub fn busy(&self) -> bool {
        self.phase != 0
    }
    pub fn begin(
        &mut self,
        source: *mut u8,
        frames: u32,
        destination: u32,
        descriptor: u32,
        word_pointer: u32,
    ) {
        self.source = source;
        self.frames = frames;
        self.destination = destination;
        self.descriptor = descriptor;
        self.fields[..4].copy_from_slice(&word_pointer.to_le_bytes());
        self.fields[4..8].copy_from_slice(&frames.to_le_bytes());
        self.fields[8..].copy_from_slice(&48000u32.to_le_bytes());
        self.phase = 1;
        self.status = 2;
        self.started = false;
        self.ticks = 0;
    }
    /// Cancel publication, but keep polling an accepted transfer to establish quiescence.
    pub fn cancel(&mut self, error: i32) {
        self.status = error;
        if !self.started {
            self.phase = 0;
        }
    }
    pub fn step(&mut self, link: &mut impl Link, guard: u32) {
        if !self.busy() {
            return;
        }
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks > guard && self.status >= 0 {
            self.cancel(-35);
        }
        if !self.busy() {
            return;
        }
        if self.started {
            match link.poll() {
                None => return,
                Some(ok) => {
                    self.started = false;
                    if !ok || self.status < 0 {
                        if self.status >= 0 {
                            self.status = -35;
                        }
                        self.phase = 0;
                        return;
                    }
                    self.phase += 1;
                    self.ticks = 0;
                    if self.phase == 4 {
                        self.phase = 0;
                        self.status = 3;
                    }
                    return;
                }
            }
        }
        let (dst, src, len) = match self.phase {
            1 => (self.destination, self.source as *const u8, self.frames * 4),
            2 => (self.descriptor + 4, self.fields.as_ptr(), 12),
            _ => (self.descriptor, self.ready.as_ptr(), 4),
        };
        let r = link.send(dst, src, len);
        if r == 0 {
            self.started = true;
        } else if r == -2 {
            self.status = -35;
            self.phase = 0;
        }
    }
    /// Caller may free this only after the transport is demonstrably terminal.
    pub fn take_source(&mut self) -> *mut u8 {
        if self.busy() {
            return core::ptr::null_mut();
        }
        core::mem::replace(&mut self.source, core::ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    struct Fake {
        busy: bool,
        done: Option<bool>,
        writes: std::vec::Vec<(u32, std::vec::Vec<u8>)>,
        retained: Option<(*const u8, usize)>,
        reads: usize,
    }
    impl Link for Fake {
        fn send(&mut self, d: u32, p: *const u8, n: u32) -> i32 {
            if self.busy {
                return -1;
            }
            self.writes.push((d, std::vec::Vec::new()));
            self.retained = Some((p, n as usize));
            self.busy = true;
            0
        }
        fn poll(&mut self) -> Option<bool> {
            // Model DSP-link reading the retained caller pointer on later ticks,
            // including after a UI timeout and on a terminal acknowledgement.
            if let Some((p, n)) = self.retained {
                self.writes.last_mut().unwrap().1 =
                    unsafe { core::slice::from_raw_parts(p, n) }.to_vec();
                self.reads += 1;
            }
            if self.done.is_some() {
                self.busy = false;
                self.retained = None;
            }
            self.done.take()
        }
    }
    fn fake() -> Fake {
        Fake {
            busy: false,
            done: None,
            writes: std::vec::Vec::new(),
            retained: None,
            reads: 0,
        }
    }
    #[test]
    fn bank_bounds_and_initial_descriptor() {
        let mut r = [0u8; 32];
        let mut image = [0u8; 32];
        for (i, v) in [(0, 1u32), (8, 32), (12, 8192), (16, 16)] {
            r[i..i + 4].copy_from_slice(&v.to_le_bytes());
        }
        assert!(valid_bank(&r, &image, 32768));
        assert!(!valid_bank(&r, &image, 32764));
        image[20] = 1;
        assert!(!valid_bank(&r, &image, 32768));
        image[20] = 0;
        r[8..12].copy_from_slice(&28u32.to_le_bytes());
        assert!(!valid_bank(&r, &image, 32768));
        r[8..12].copy_from_slice(&32u32.to_le_bytes());
        r[12..16].copy_from_slice(&8193u32.to_le_bytes());
        assert!(!valid_bank(&r, &image, 32772));
    }
    #[test]
    fn track_table_bounds_zero_and_overlap() {
        let mut r = [0u8; 32];
        let image = [0u8; 1024];
        for (i, v) in [(0, 2u32), (8, 0), (12, 512)] {
            r[i..i + 4].copy_from_slice(&v.to_le_bytes());
        }
        assert!(valid_tracks(&r, &image));
        let set = |r: &mut [u8; 32], i: usize, v: u32| r[i..i + 4].copy_from_slice(&v.to_le_bytes());
        for (word, value) in [(0, 1u32), (8, 4), (8, 513), (12, 770), (12, 1022), (12, 2), (16, 1), (28, 1), (8, u32::MAX - 3)] {
            let mut bad = r;
            set(&mut bad, word, value);
            assert!(!valid_tracks(&bad, &image), "word {word} = {value}");
        }
        let mut dirty = image;
        dirty[600] = 1;
        assert!(!valid_tracks(&r, &dirty));
        assert!(!valid_tracks(&r, &image[..767]));
    }
    #[test]
    fn publication_waits_for_each_ack() {
        let mut data = [0u8; 8];
        let mut j = Job::new();
        let mut l = fake();
        j.begin(data.as_mut_ptr(), 2, 100, 20, 25);
        assert!(j.take_source().is_null());
        j.step(&mut l, 100);
        j.step(&mut l, 100);
        assert_eq!(l.writes.len(), 1);
        assert_eq!(j.status, 2);
        l.done = Some(true);
        j.step(&mut l, 100);
        j.step(&mut l, 100);
        assert!(j.take_source().is_null());
        j.step(&mut l, 100);
        assert_eq!(
            l.writes[1],
            (
                24,
                [25u32, 2, 48000]
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .collect()
            )
        );
        assert_eq!(j.status, 2);
        l.done = Some(true);
        j.step(&mut l, 100);
        j.step(&mut l, 100);
        assert!(j.take_source().is_null());
        j.step(&mut l, 100);
        assert_eq!(l.writes[2], (20, std::vec![1, 0, 0, 0]));
        assert_eq!(j.status, 2);
        l.done = Some(true);
        j.step(&mut l, 100);
        assert_eq!(j.status, 3);
        assert_eq!(j.take_source(), data.as_mut_ptr());
        assert!(j.take_source().is_null());
    }
    #[test]
    fn timeout_retains_source_until_resumed_terminal_poll() {
        let mut data = [7u8; 4];
        let mut j = Job::new();
        let mut l = fake();
        j.begin(data.as_mut_ptr(), 1, 100, 20, 25);
        j.step(&mut l, 1);
        j.step(&mut l, 1);
        assert_eq!(j.status, -35);
        assert!(j.busy());
        assert!(j.take_source().is_null());
        j.step(&mut l, 1);
        assert_eq!(l.writes.len(), 1);
        assert_eq!(l.writes[0].1, std::vec![7; 4]);
        assert_eq!(l.reads, 2);
        l.done = Some(true);
        j.step(&mut l, 1);
        assert!(!j.busy());
        assert_eq!(j.take_source(), data.as_mut_ptr());
        assert_eq!(l.writes.len(), 1);
    }
    #[test]
    fn busy_and_cancel_never_publish() {
        let mut data = [0u8; 4];
        let mut j = Job::new();
        let mut l = fake();
        l.busy = true;
        j.begin(data.as_mut_ptr(), 1, 100, 20, 25);
        j.step(&mut l, 100);
        assert!(l.writes.is_empty());
        l.busy = false;
        j.step(&mut l, 100);
        j.cancel(-1);
        assert!(j.take_source().is_null());
        l.done = Some(false);
        j.step(&mut l, 100);
        assert_eq!(j.status, -1);
        assert!(!j.take_source().is_null());
        assert_eq!(l.writes.len(), 1);
    }
    #[test]
    fn lost_ready_ack_retains_source_and_does_not_resend() {
        let mut data = [9u8; 4];
        let mut j = Job::new();
        let mut l = fake();
        j.begin(data.as_mut_ptr(), 1, 100, 20, 25);
        for _ in 0..2 {
            j.step(&mut l, 3);
            assert!(j.take_source().is_null());
            l.done = Some(true);
            j.step(&mut l, 3);
            assert!(j.take_source().is_null());
        }
        j.step(&mut l, 3); // READY accepted; its acknowledgement is lost.
        assert_eq!(l.writes.len(), 3);
        assert!(j.take_source().is_null());
        for _ in 0..4 {
            j.step(&mut l, 3);
        }
        assert_eq!(j.status, -35);
        assert!(j.busy());
        assert!(j.take_source().is_null());
        assert_eq!(l.writes[2], (20, std::vec![1, 0, 0, 0]));
        let reads = l.reads;
        l.done = Some(true);
        j.step(&mut l, 3); // Transport resumes and reads READY's retained buffer.
        assert_eq!(l.reads, reads + 1);
        assert_eq!(j.status, -35);
        assert!(!j.busy());
        assert_eq!(j.take_source(), data.as_mut_ptr());
        j.step(&mut l, 3);
        assert_eq!(l.writes.len(), 3);
    }
    #[test]
    fn terminal_failure_in_each_phase_stops_publication() {
        for failed_phase in 0..3 {
            let mut data = [5u8; 4];
            let mut j = Job::new();
            let mut l = fake();
            j.begin(data.as_mut_ptr(), 1, 100, 20, 25);
            for _ in 0..failed_phase {
                j.step(&mut l, 100);
                l.done = Some(true);
                j.step(&mut l, 100);
            }
            j.step(&mut l, 100);
            assert!(j.take_source().is_null());
            l.done = Some(false);
            j.step(&mut l, 100);
            assert_eq!(j.status, -35);
            assert!(!j.busy());
            assert_eq!(j.take_source(), data.as_mut_ptr());
            j.step(&mut l, 100);
            assert_eq!(l.writes.len(), failed_phase + 1);
        }
    }
    #[test]
    fn cancel_before_acceptance_releases_without_publication() {
        let mut data = [5u8; 4];
        let mut j = Job::new();
        let mut l = fake();
        j.begin(data.as_mut_ptr(), 1, 100, 20, 25);
        j.cancel(-1);
        j.step(&mut l, 100);
        assert_eq!(j.status, -1);
        assert_eq!(j.take_source(), data.as_mut_ptr());
        assert!(l.writes.is_empty());
    }
}
