extern crate std;
use super::*;
use std::vec::Vec;

#[derive(Default)]
struct Fake {
    files: Vec<(Vec<u8>, FileId, u32, u32)>, // path, id, frames, channels
    free: u32,
    queued: Vec<u32>,
    cancelled: Vec<u32>,
    sync: bool,
    loads: u32,
    released: u32,
    lost: bool,
    closed: [[bool; 16]; 2],
    drain: bool,
    published: Vec<(usize, usize, Strack)>,
    next_tok: u32,
    load_err: Option<i32>,
    drain_forever: bool,
    /// Ticks the publish writes take to land (0: at once), the ticks left for the one in flight, and a fixed failure code.
    land_after: u32,
    landing: u32,
    publish_err: i32,
    log: Vec<&'static str>,
}
fn id(size: u32, hash: u8) -> FileId {
    let mut h = [0; 32];
    h[0] = hash;
    FileId { root: 1, size, mtime: 0, flags: ID_HASH_VALID, hash_kind: 1, hash: h }
}
impl Fake {
    fn new() -> Self {
        Fake { free: 100_000, sync: true, drain: true, next_tok: 1, ..Default::default() }
    }
    fn add(&mut self, p: &str, i: FileId, frames: u32, ch: u32) {
        self.files.push((p.as_bytes().to_vec(), i, frames, ch));
    }
    fn file(&self, p: &[u8]) -> Option<&(Vec<u8>, FileId, u32, u32)> {
        self.files.iter().find(|f| f.0 == p)
    }
}
impl Env for Fake {
    fn inspect(&mut self, p: &[u8]) -> Result<Inspect, i32> {
        self.log.push("inspect");
        self.file(p).map(|f| Inspect { id: f.1, frames: f.2, rate: 48000, channels: f.3 }).ok_or(E_NOT_FOUND)
    }
    fn stat(&mut self, p: &[u8]) -> Result<FileId, i32> {
        self.file(p).map(|f| f.1).ok_or(E_NOT_FOUND)
    }
    fn load(&mut self, req: u32, i: &FileId, p: &[u8], out: &mut Handle) -> Load {
        if let Some(e) = self.load_err {
            return Load::Err(e);
        }
        let f = self.file(p).unwrap().clone();
        assert!(f.1.same(i));
        self.loads += 1;
        *out = Handle { id: self.loads, token: self.next_tok, base_l: 0x1000 * self.loads, base_r: 0x1000 * self.loads + f.2, frames: f.2, rate: 48000, channels: f.3 };
        self.next_tok += 1;
        if self.sync {
            Load::Done
        } else {
            self.queued.push(req);
            Load::Queued
        }
    }
    fn release(&mut self, _h: &Handle) {
        self.released += 1;
    }
    fn cancel(&mut self, req: u32) {
        self.cancelled.push(req);
    }
    fn check(&mut self, _h: &Handle) -> i32 {
        if self.lost { E_LOST } else { 0 }
    }
    fn free_frames(&mut self) -> u32 {
        self.free
    }
    fn close(&mut self, c: usize, k: usize) {
        self.log.push("close");
        self.closed[c][k] = true;
    }
    fn drained(&mut self, _c: usize, _k: usize) -> bool {
        self.drain && !self.drain_forever
    }
    fn publish(&mut self, c: usize, k: usize, r: &Strack) {
        self.closed[c][k] = false;
        self.published.push((c, k, *r));
        self.landing = self.land_after;
    }
    fn published(&mut self, _c: usize, _k: usize) -> i32 {
        if self.publish_err != 0 {
            return self.publish_err;
        }
        if self.landing == 0 {
            return 1;
        }
        self.landing -= 1;
        0
    }
}
type C = Core<2, 8, 3>;
fn run(c: &mut C, e: &mut Fake, n: usize) {
    for _ in 0..n {
        c.tick(e);
    }
}
fn core() -> C {
    let mut c = C::new();
    assert!(c.open(0));
    c
}

#[test]
fn select_reaches_ready_and_publishes_one_record() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    assert_eq!(c.select(0, 3, b"a.wav\0junk"), 0);
    assert_eq!(c.status(0, 3), QUEUED);
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 3), READY);
    let (cl, tr, r) = e.published[0];
    assert_eq!((cl, tr), (0, 3));
    assert_eq!((r.ready, r.frames, r.channels, r.fmt), (1, 500, 2, FMT_PCM16));
    assert_ne!(r.base_l, r.base_r);
    assert_eq!(c.info(0, 3).unwrap().token, r.token);
}

#[test]
fn ready_waits_until_the_record_has_landed_in_the_dsp() {
    let (mut c, mut e) = (core(), Fake::new());
    e.land_after = 3;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), LOADING, "published, but the writes are on their way");
    assert_eq!(c.info(0, 0), Err(E_NOT_FOUND));
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(e.published.len(), 1);
}

#[test]
fn a_publish_the_link_gave_up_is_a_track_error_and_a_new_select_recovers() {
    let (mut c, mut e) = (core(), Fake::new());
    e.publish_err = E_DSP_XFER;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 6);
    assert_eq!(c.status(0, 0), E_DSP_XFER);
    e.publish_err = 0;
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 8);
    assert_eq!(c.status(0, 0), READY);
}

#[test]
fn backing_lost_while_the_record_is_on_its_way_reloads() {
    let (mut c, mut e) = (core(), Fake::new());
    e.land_after = 2;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), LOADING);
    c.lost(&mut e);
    run(&mut c, &mut e, 20);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(e.loads, 2, "reloaded from the remembered path");
}

#[test]
fn bad_arguments() {
    let (mut c, _e) = (core(), Fake::new());
    assert_eq!(c.select(0, 16, b"a"), E_CONTRACT);
    assert_eq!(c.select(0, 0, b""), E_SEL_PATH);
    assert_eq!(c.select(0, 0, &[b'a'; 256]), E_SEL_PATH);
    assert_eq!(c.select(1, 0, b"a"), E_NOT_FOUND);
    assert_eq!(c.status(0, 16), E_CONTRACT);
    assert_eq!(c.info(0, 0), Err(E_NOT_FOUND));
}

#[test]
fn mono_publishes_both_planes_at_one_address() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("m.wav", id(10, 2), 64, 1);
    c.select(0, 0, b"m.wav");
    run(&mut c, &mut e, 4);
    let r = e.published[0].2;
    assert_eq!(r.base_l, r.base_r);
}

#[test]
fn preflight_errors_leave_the_old_binding() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("big.wav", id(900, 3), 90_000, 2);
    e.add("zero.wav", id(5, 4), 0, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    c.select(0, 0, b"nope.wav");
    run(&mut c, &mut e, 2);
    assert_eq!(c.status(0, 0), E_NOT_FOUND);
    assert_eq!(c.refs(0, 0), Some(1));
    assert!(!e.closed[0][0]);
    c.select(0, 1, b"zero.wav");
    run(&mut c, &mut e, 2);
    assert_eq!(c.status(0, 1), E_FORMAT);
    c.select(0, 1, b"big.wav");
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 1), E_TOO_BIG);
    assert_eq!(e.loads, 1);
    // ERROR is sticky until the next select
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 1), E_TOO_BIG);
}

#[test]
fn dedup_shares_one_load_and_the_last_unbind_releases() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"a.wav");
    run(&mut c, &mut e, 6);
    assert_eq!((c.status(0, 0), c.status(0, 1)), (READY, READY));
    assert_eq!(e.loads, 1);
    assert_eq!(c.refs(0, 0), Some(2));
    let toks: Vec<u32> = e.published.iter().map(|p| p.2.token).collect();
    assert_ne!(toks[0], toks[1]);
    c.unbind(0, 0);
    assert_eq!(c.status(0, 0), UNUSED);
    run(&mut c, &mut e, 4);
    assert_eq!(e.released, 0);
    c.unbind(0, 1);
    run(&mut c, &mut e, 4);
    assert_eq!(e.released, 1);
    assert_eq!(c.entries_used(), 0);
}

#[test]
fn identity_without_hash_needs_the_same_path() {
    let (mut c, mut e) = (core(), Fake::new());
    let mut i = id(100, 0);
    i.flags = 0;
    i.mtime = 7;
    e.add("a.wav", i, 500, 2);
    e.add("b.wav", i, 500, 2);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"b.wav");
    c.select(0, 2, b"a.wav");
    run(&mut c, &mut e, 8);
    assert_eq!(e.loads, 2);
}

#[test]
fn reselect_closes_and_drains_before_the_backing_is_touched() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 1);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    e.drain = false;
    c.select(0, 0, b"b.wav");
    run(&mut c, &mut e, 5);
    assert!(e.closed[0][0]);
    assert_eq!(c.status(0, 0), QUEUED);
    assert_eq!((e.loads, e.released), (1, 0));
    e.drain = true;
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!((e.loads, e.released), (2, 1));
    assert_eq!(c.info(0, 0).unwrap().channels, 1);
    assert!(!e.closed[0][0]);
}

#[test]
fn reselect_of_the_same_file_keeps_its_entry() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    let t1 = c.info(0, 0).unwrap().token;
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 6);
    assert_eq!((e.loads, e.released), (1, 0));
    assert_ne!(c.info(0, 0).unwrap().token, t1);
}

#[test]
fn drain_that_never_acks_fails_closed_and_keeps_the_pcm() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 1);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    e.drain_forever = true;
    c.select(0, 0, b"b.wav");
    run(&mut c, &mut e, DRAIN_GUARD as usize + 10);
    assert_eq!(c.status(0, 0), E_DSP_XFER);
    assert_eq!(e.released, 0);
}

#[test]
fn supersede_while_loading_drops_the_old_result() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), LOADING);
    c.select(0, 0, b"b.wav");
    assert_eq!(c.status(0, 0), QUEUED);
    run(&mut c, &mut e, 3);
    assert_eq!(e.queued.len(), 2);
    // the first completion arrives late: the track must not publish it
    c.done(e.queued[0], 0);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), LOADING);
    assert!(e.published.is_empty());
    assert_eq!(e.released, 1, "orphan entry released once terminal");
    c.done(e.queued[1], 0);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(c.info(0, 0).unwrap().frames, 300);
    c.done(9999, 0);
    c.done(0, 0);
}

#[test]
fn load_errors_become_sticky_track_errors() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    c.done(e.queued[0], E_POOL);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), E_POOL);
    assert_eq!(c.entries_used(), 0);
    e.load_err = Some(E_POOL);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), E_POOL);
    assert_eq!(c.entries_used(), 0);
}

#[test]
fn file_changed_between_select_and_load_is_refused() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    e.files[0].1 = id(100, 9);
    c.done(e.queued[0], 0);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), E_CHANGED);
    assert!(e.published.is_empty());
}

#[test]
fn backing_lost_reloads_bound_tracks() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"a.wav");
    run(&mut c, &mut e, 6);
    let t0 = c.info(0, 0).unwrap().token;
    c.lost(&mut e);
    assert_eq!(c.status(0, 0), LOADING);
    run(&mut c, &mut e, 8);
    assert_eq!((c.status(0, 0), c.status(0, 1)), (READY, READY));
    assert_eq!(e.loads, 2, "one reload shared by both tracks");
    assert_eq!(e.released, 0, "dead handles are never released");
    assert_ne!(c.info(0, 0).unwrap().token, t0);
}

#[test]
fn backing_lost_closes_a_playing_track_before_any_file_access() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 6);
    assert_eq!(c.status(0, 0), READY);
    e.log.clear();
    c.lost(&mut e);
    assert_eq!(e.log, ["close"], "the close is issued by lost() itself");
    run(&mut c, &mut e, 8);
    assert_eq!(e.log[1], "inspect");
    assert_eq!(c.status(0, 0), READY);
}

#[test]
fn backing_lost_with_a_changed_or_missing_file() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(100, 2), 500, 2);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"b.wav");
    run(&mut c, &mut e, 6);
    e.files[0].1 = id(100, 7);
    e.files.pop();
    c.lost(&mut e);
    run(&mut c, &mut e, 8);
    assert_eq!(c.status(0, 0), E_CHANGED);
    assert_eq!(c.status(0, 1), E_LOST);
}

#[test]
fn check_failure_before_publish_counts_as_loss() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    e.lost = true;
    run(&mut c, &mut e, 2);
    assert_eq!(c.status(0, 0), LOADING);
    e.lost = false;
    run(&mut c, &mut e, 8);
    assert_eq!(c.status(0, 0), READY);
}

#[test]
fn project_change_selects_first_then_unbinds_and_keeps_shared_entries() {
    let (mut c, mut e) = (core(), Fake::new());
    for (n, h) in [("a.wav", 1u8), ("b.wav", 2), ("c.wav", 3)] {
        e.add(n, id(100 + h as u32, h), 400, 2);
    }
    c.project_changed(5);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"b.wav");
    run(&mut c, &mut e, 8);
    c.project_changed(6);
    c.select(0, 0, b"a.wav");
    c.select(0, 2, b"c.wav");
    run(&mut c, &mut e, 8);
    assert_eq!(e.loads, 3);
    assert_eq!(c.status(0, 1), UNUSED, "project 6 has no track 1");
    c.project_changed(5);
    run(&mut c, &mut e, 10);
    assert_eq!((c.status(0, 0), c.status(0, 1), c.status(0, 2)), (READY, READY, UNUSED));
    assert_eq!(e.loads, 4, "a.wav survived through dedup; b.wav was released by project 6 and loads again");
}

#[test]
fn selections_made_before_the_project_is_known_belong_to_the_first_project() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 400, 2);
    c.select(0, 4, b"a.wav");
    c.project_changed(2);
    run(&mut c, &mut e, 5);
    assert_eq!(c.status(0, 4), READY);
    c.project_changed(3);
    run(&mut c, &mut e, 5);
    assert_eq!(c.status(0, 4), UNUSED);
    c.project_changed(2);
    run(&mut c, &mut e, 6);
    assert_eq!(c.status(0, 4), READY);
}

#[test]
fn missing_file_after_project_change_is_a_per_track_error() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 400, 2);
    c.project_changed(1);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"gone.wav");
    c.project_changed(2);
    c.project_changed(1);
    run(&mut c, &mut e, 8);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(c.status(0, 1), E_NOT_FOUND);
}

#[test]
fn teardown_waits_for_the_drain_then_frees() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 400, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    e.drain = false;
    assert!(!c.teardown(0, &mut e));
    e.drain = true;
    assert!(c.teardown(0, &mut e) || c.teardown(0, &mut e));
    assert!(!c.is_open(0));
    run(&mut c, &mut e, 2);
    assert_eq!((e.released, c.entries_used()), (1, 0));
}

#[test]
fn tokens_and_requests_do_not_wrap() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 400, 2);
    c.next_token = u32::MAX;
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 0), E_NOMEM);
    let mut c = core();
    c.next_req = u32::MAX;
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 0), E_NOMEM);
}

#[test]
fn a_select_that_supersedes_a_running_load_cancels_it_once() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    assert!(e.cancelled.is_empty());
    c.select(0, 0, b"b.wav");
    run(&mut c, &mut e, 3);
    assert_eq!(e.cancelled, [e.queued[0]], "the request of the superseded load");
    run(&mut c, &mut e, 5);
    assert_eq!(e.cancelled.len(), 1, "once");
    // the adapter still completes the cancelled request; the entry is reaped, the track is not touched
    c.done(e.queued[0], E_LOST);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), LOADING);
    assert_eq!(c.entries_used(), 2 - 1, "the cancelled entry is free again");
    c.done(e.queued[1], 0);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(c.info(0, 0).unwrap().frames, 300);
}

#[test]
fn a_cancelled_load_is_not_shared_with_a_new_select_of_the_same_file() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 3);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    assert_eq!(e.cancelled.len(), 1);
    assert_eq!(e.queued.len(), 2, "a new load, not the cancelled one");
    c.done(e.queued[0], E_LOST);
    c.done(e.queued[1], 0);
    run(&mut c, &mut e, 4);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!((e.loads, e.released), (2, 0));
}

#[test]
fn a_load_another_track_shares_is_not_cancelled() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 2);
    c.select(0, 0, b"a.wav");
    c.select(0, 1, b"a.wav");
    run(&mut c, &mut e, 4);
    assert_eq!(e.queued.len(), 1, "dedup: one load for both tracks");
    c.select(0, 0, b"b.wav");
    run(&mut c, &mut e, 4);
    assert!(e.cancelled.is_empty(), "track 1 still waits for it");
    c.unbind(0, 1);
    run(&mut c, &mut e, 4);
    assert_eq!(e.cancelled, [e.queued[0]], "the last waiter went");
}

#[test]
fn unbind_of_a_loading_track_cancels_its_load() {
    let (mut c, mut e) = (core(), Fake::new());
    e.sync = false;
    e.add("a.wav", id(100, 1), 500, 2);
    c.select(0, 2, b"a.wav");
    run(&mut c, &mut e, 3);
    c.unbind(0, 2);
    run(&mut c, &mut e, 3);
    assert_eq!(e.cancelled, [e.queued[0]]);
    assert_eq!(c.status(0, 2), UNUSED);
}

#[test]
fn a_finished_load_is_never_cancelled() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a.wav", id(100, 1), 500, 2);
    e.add("b.wav", id(200, 2), 300, 2);
    c.select(0, 0, b"a.wav");
    run(&mut c, &mut e, 4);
    c.select(0, 0, b"b.wav");
    run(&mut c, &mut e, 4);
    assert!(e.cancelled.is_empty());
}

#[test]
fn the_track_path_is_there_in_every_state_but_unused() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("dir/a.wav", id(100, 1), 500, 2);
    assert_eq!(c.path(0, 0), Err(E_NOT_FOUND));
    assert_eq!(c.path(0, 16), Err(E_CONTRACT));
    assert_eq!(c.path(1, 0), Err(E_NOT_FOUND));
    c.select(0, 0, b"dir/a.wav");
    assert_eq!(c.path(0, 0), Ok(&b"dir/a.wav"[..]), "queued");
    run(&mut c, &mut e, 4);
    assert_eq!(c.path(0, 0), Ok(&b"dir/a.wav"[..]), "ready");
    c.select(0, 1, b"dir/missing.wav");
    run(&mut c, &mut e, 2);
    assert_eq!(c.status(0, 1), E_NOT_FOUND);
    assert_eq!(c.path(0, 1), Ok(&b"dir/missing.wav"[..]), "a sticky error still names its file");
    c.unbind(0, 0);
    assert_eq!(c.path(0, 0), Err(E_NOT_FOUND));
}

#[test]
fn the_track_path_follows_a_project_change() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("p1.wav", id(100, 1), 500, 2);
    e.add("p2.wav", id(200, 2), 300, 2);
    c.project_changed(1);
    c.select(0, 0, b"p1.wav");
    run(&mut c, &mut e, 4);
    c.project_changed(2);
    assert_eq!(c.path(0, 0), Err(E_NOT_FOUND), "the new project has no selection on track 0");
    c.select(0, 0, b"p2.wav");
    c.project_changed(1);
    assert_eq!(c.path(0, 0), Ok(&b"p1.wav"[..]));
}

#[test]
fn pool_add_takes_the_first_free_index_and_a_cleared_hole_is_taken_next() {
    let (mut c, mut e) = (core(), Fake::new());
    for (i, n) in ["a", "b", "c"].iter().enumerate() {
        e.add(n, id(100 + i as u32, i as u8 + 1), 500, 2);
    }
    assert_eq!(c.pool_add(0, b"a"), 0);
    assert_eq!(c.pool_add(0, b"b"), 1);
    assert_eq!(c.pool_add(0, b"c"), 2);
    run(&mut c, &mut e, 6);
    assert_eq!((c.status(0, 0), c.status(0, 1), c.status(0, 2)), (READY, READY, READY));
    // a hole at 1 keeps indexes 0 and 2 where they are; the next add fills it
    assert_eq!(c.unbind(0, 1), 0);
    run(&mut c, &mut e, 4);
    assert_eq!((c.status(0, 0), c.status(0, 1), c.status(0, 2)), (READY, UNUSED, READY));
    e.add("d", id(104, 4), 500, 2);
    assert_eq!(c.pool_add(0, b"d"), 1);
    assert_eq!(c.path(0, 1), Ok(&b"d"[..]));
    assert_eq!(c.path(0, 2), Ok(&b"c"[..]));
}

#[test]
fn pool_add_of_a_file_already_in_the_pool_returns_its_index() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a", id(100, 1), 500, 2);
    e.add("b", id(101, 2), 500, 2);
    assert_eq!(c.pool_add(0, b"a"), 0);
    assert_eq!(c.pool_add(0, b"b"), 1);
    run(&mut c, &mut e, 6);
    assert_eq!(c.pool_add(0, b"a"), 0, "same path, same entry, no second load");
    assert_eq!(c.pool_add(0, b"b\0junk"), 1);
    run(&mut c, &mut e, 6);
    assert_eq!(e.loads, 2);
}

#[test]
fn pool_add_of_a_file_with_a_sticky_error_selects_it_again() {
    let (mut c, mut e) = (core(), Fake::new());
    assert_eq!(c.pool_add(0, b"late"), 0);
    run(&mut c, &mut e, 3);
    assert_eq!(c.status(0, 0), E_NOT_FOUND);
    e.add("late", id(100, 1), 500, 2);
    assert_eq!(c.pool_add(0, b"late"), 0, "the same entry");
    run(&mut c, &mut e, 6);
    assert_eq!(c.status(0, 0), READY);
}

#[test]
fn pool_add_reports_a_full_pool_and_bad_arguments() {
    let (mut c, mut e) = (core(), Fake::new());
    for i in 0..TRACKS {
        let name = std::format!("f{i}");
        e.add(&name, id(100 + i as u32, i as u8 + 1), 100, 1);
        assert_eq!(c.pool_add(0, name.as_bytes()), i as i32);
    }
    e.add("extra", id(999, 99), 100, 1);
    assert_eq!(c.pool_add(0, b"extra"), E_POOL);
    assert_eq!(c.pool_add(0, b"f3"), 3, "a file already in a full pool still resolves");
    assert_eq!(c.pool_add(0, b""), E_SEL_PATH);
    assert_eq!(c.pool_add(1, b"f1"), E_NOT_FOUND, "client 1 is not open");
    assert_eq!(c.unbind(0, 7), 0);
    assert_eq!(c.pool_add(0, b"extra"), 7);
}

#[test]
fn pool_set_fills_an_empty_entry_at_the_given_index() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a", id(100, 1), 500, 2);
    assert_eq!(c.pool_set(0, 5, b"a"), 0);
    assert_eq!(c.status(0, 4), UNUSED);
    run(&mut c, &mut e, 6);
    assert_eq!(c.status(0, 5), READY);
    assert_eq!(c.path(0, 5), Ok(&b"a"[..]));
    assert_eq!(c.entries_used(), 1);
}

#[test]
fn pool_set_replaces_a_filled_entry_and_releases_the_old_one() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a", id(100, 1), 500, 2);
    e.add("b", id(101, 2), 500, 2);
    assert_eq!(c.pool_set(0, 0, b"a"), 0);
    run(&mut c, &mut e, 6);
    assert_eq!((c.status(0, 0), c.entries_used()), (READY, 1));
    assert_eq!(c.pool_set(0, 0, b"b"), 0);
    run(&mut c, &mut e, 10);
    assert_eq!(c.status(0, 0), READY);
    assert_eq!(c.path(0, 0), Ok(&b"b"[..]));
    assert_eq!(c.entries_used(), 1, "the old file is released");
}

#[test]
fn pool_set_of_one_file_into_two_entries_loads_it_once() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a", id(100, 1), 500, 2);
    assert_eq!(c.pool_set(0, 0, b"a"), 0);
    run(&mut c, &mut e, 6);
    assert_eq!(c.pool_set(0, 3, b"a"), 0);
    run(&mut c, &mut e, 6);
    assert_eq!((c.status(0, 0), c.status(0, 3)), (READY, READY));
    assert_eq!((e.loads, c.entries_used()), (1, 1));
    assert_eq!(c.refs(0, 0), Some(2));
}

#[test]
fn pool_set_reports_bad_arguments() {
    let (mut c, mut e) = (core(), Fake::new());
    e.add("a", id(100, 1), 500, 2);
    assert_eq!(c.pool_set(0, TRACKS, b"a"), E_CONTRACT);
    assert_eq!(c.pool_set(0, 0, b""), E_SEL_PATH);
    assert_eq!(c.pool_set(1, 0, b"a"), E_NOT_FOUND, "client 1 is not open");
    assert_eq!(c.status(0, 0), UNUSED);
}

#[test]
fn the_pool_list_follows_a_project_change_with_its_holes() {
    let (mut c, mut e) = (core(), Fake::new());
    for (n, h) in [("a", 1u8), ("b", 2), ("c", 3), ("x", 4)] {
        e.add(n, id(100 + h as u32, h), 500, 2);
    }
    c.project_changed(1);
    assert_eq!(c.pool_add(0, b"a"), 0);
    assert_eq!(c.pool_add(0, b"b"), 1);
    assert_eq!(c.pool_add(0, b"c"), 2);
    run(&mut c, &mut e, 8);
    c.unbind(0, 1);
    run(&mut c, &mut e, 4);
    c.project_changed(2);
    run(&mut c, &mut e, 8);
    assert_eq!(c.status(0, 0), UNUSED, "project 2 starts with an empty pool");
    assert_eq!(c.pool_add(0, b"x"), 0);
    run(&mut c, &mut e, 6);
    c.project_changed(1);
    run(&mut c, &mut e, 12);
    assert_eq!((c.status(0, 0), c.status(0, 1), c.status(0, 2)), (READY, UNUSED, READY), "the hole at 1 is still a hole");
    assert_eq!(c.path(0, 2), Ok(&b"c"[..]));
}

#[test]
fn paths_get_the_device_root() {
    let mut out = [0u8; PATH];
    let n = rooted(ROOT_DT2, b"KICKS/K1", &mut out).unwrap();
    assert_eq!(&out[..n], b"/KICKS/K1");
    let n = rooted(ROOT_DT2, b"/KICKS/K1\0junk", &mut out).unwrap();
    assert_eq!(&out[..n], b"/KICKS/K1", "the 1.5 absolute form stays as it is");
    let n = rooted(ROOT_DN2, b"KICKS/K1", &mut out).unwrap();
    assert_eq!(&out[..n], b"samples/KICKS/K1");
    let n = rooted(ROOT_DN2, b"samples/x", &mut out).unwrap();
    assert_eq!(&out[..n], b"samples/samples/x", "root-relative always on DN2: a folder may be called samples");
    assert_eq!(rooted(ROOT_DT2, b"", &mut out), Err(E_SEL_PATH));
    assert_eq!(rooted(ROOT_DT2, b"\0a", &mut out), Err(E_SEL_PATH));
    // the result must fit: 255 bytes is the most
    assert!(rooted(ROOT_DT2, &[b'a'; 254], &mut out).is_ok());
    assert_eq!(rooted(ROOT_DT2, &[b'a'; 255], &mut out), Err(E_SEL_PATH));
    assert!(rooted(ROOT_DN2, &[b'a'; 247], &mut out).is_ok());
    assert_eq!(rooted(ROOT_DN2, &[b'a'; 248], &mut out), Err(E_SEL_PATH));
}

#[test]
fn stored_paths_lose_the_device_root() {
    assert_eq!(unrooted(ROOT_DT2, b"/KICKS/K1"), b"KICKS/K1");
    assert_eq!(unrooted(ROOT_DN2, b"samples/KICKS/K1"), b"KICKS/K1");
    assert_eq!(unrooted(ROOT_DN2, b"other/x"), b"other/x");
    // a listing path survives the round trip on both devices
    let mut out = [0u8; PATH];
    for root in [ROOT_DT2, ROOT_DN2] {
        let n = rooted(root, b"A/B.wav", &mut out).unwrap();
        assert_eq!(unrooted(root, &out[..n]), b"A/B.wav");
    }
}

#[test]
fn a_store_file_is_inspected_with_the_native_header_and_its_table_sha() {
    // 64-byte header: channels at byte 1 (0 mono, 1 stereo), PCM bytes at 4..8, rate at 8..12, 0x7f at 0x14
    let mut head = [0u8; 64];
    head[1] = 1;
    head[4..8].copy_from_slice(&400u32.to_be_bytes());
    head[8..12].copy_from_slice(&48000u32.to_be_bytes());
    head[0x14] = 0x7f;
    let sha = [7u8; 32];
    let size = 64 + 400 + 16;
    let i = store_inspect(size, &sha, &head).unwrap();
    assert_eq!((i.frames, i.channels, i.rate), (100, 2, 48000));
    assert_eq!((i.id.size, i.id.hash_kind, i.id.flags), (size, crate::files::HASH_SHA256, ID_HASH_VALID));
    assert_eq!(i.id.hash, sha);
    assert!(i.id.same(&store_identity(size, &sha)));
    assert!(!i.id.same(&store_identity(size, &[8u8; 32])), "another sha is another file");
    assert!(!i.id.same(&store_identity(size + 4, &sha)));
    assert!(store_inspect(size + 4, &sha, &head).is_ok(), "extra bytes after the trailer are allowed");
    assert_eq!(store_inspect(64 + 400 - 1, &sha, &head).err(), Some(E_FORMAT), "size is shorter than the header says");
    head[8] ^= 1;
    assert_eq!(store_inspect(size, &sha, &head).err(), Some(E_FORMAT), "rate");
}
