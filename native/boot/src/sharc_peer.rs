//! Opt-in live coupling of the ColdFire DSPI2 link to the native SHARC+
//! engine (cargo feature `sharc`).
//!
//! One ColdFire DSPI2 frame arrives every 88,000 ColdFire instructions
//! (1.5 kHz, one 32-sample 48 kHz audio block). For each frame the peer
//! 1. delivers it through the engine's SPI2 DMA model and takes the reply,
//! 2. runs the DSP for `period` instructions (the DSP clock / 1.5 kHz,
//!    666,667 at an assumed 1 GHz), noting how many instructions ran before
//!    the RTOS idle loop was reached,
//! 3. completes one SPORT4 audio block and appends its 32 stereo samples to
//!    a shared PCM buffer.
//!
//! The engine is generic over [`DspEngine`] so the plumbing is testable
//! without firmware.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::mpsc, thread, time::Instant};

use periph::dspi::Peer;

/// DN2 RTOS idle loop, short-word PCs (inclusive start, exclusive end).
pub const DN2_IDLE_RANGE: (u32, u32) = (0xb8_8a49, 0xb8_8abb);
/// Head of the idle task's spin loop (a linked call to `0xb88a49`).
pub const DN2_IDLE_HEAD: u32 = 0xb8_8aab;
/// 1 GHz DSP clock / 1.5 kHz frame rate.
pub const DEFAULT_PERIOD: u32 = 666_667;
const CHUNK: u32 = 1024;
/// Words in one SPORT4A block: 32 stereo samples, L even / R odd.
pub const BLOCK_WORDS: usize = 64;

pub fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// What the peer needs from a DSP engine.
pub trait DspEngine {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String>;
    /// Run up to `n` instructions; fewer than `n` means the engine stopped.
    fn step(&mut self, n: u32) -> u32;
    /// Like `step(n)`, but also end at the first instruction boundary whose
    /// PC lies in `range` (start inclusive, end exclusive). None: not
    /// supported (the peer then steps in chunks). Results never depend on
    /// step sizes, so this only saves the chunk boundaries' cost.
    fn step_until_in(&mut self, _n: u32, _range: (u32, u32)) -> Option<u32> {
        None
    }
    /// `step_until_in` that stops only at a boundary whose instruction count
    /// is a positive multiple of `chunk` past `base_icount`, which is an
    /// `icount()` value. None: not supported (the peer steps in chunks).
    fn step_until_in_aligned(
        &mut self,
        _n: u32,
        _range: (u32, u32),
        _chunk: u32,
        _base_icount: u64,
    ) -> Option<u32> {
        None
    }
    /// One SPORT4 block (zero input); `Ok(None)` while the SPORTs are off.
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String>;
    /// Short-word PC.
    fn pc(&self) -> u32;
    /// Halt reason, once stopped.
    fn halt_reason(&self) -> Option<String>;
    /// Canonical state blob (hosts that snapshot); empty when unsupported.
    fn export(&self) -> Vec<u8> {
        Vec::new()
    }
    /// Executed-instruction counter (hosts that snapshot).
    fn icount(&self) -> u64 {
        0
    }
}

/// Per-frame record.
#[derive(Clone, Debug, Default)]
pub struct FrameStat {
    pub first_word: u16,
    /// FNV-1a 64 of the TX frame and of the reply.
    pub tx_hash: u64,
    pub reply_hash: u64,
    pub reply_nonzero_bytes: u32,
    /// DSP instructions run before the first chunk ending in the idle range
    /// (the whole period if idle was never reached).
    pub busy: u32,
    pub reached_idle: bool,
    pub executed: u32,
    /// None: SPORTs not running; zeros were appended.
    pub block: bool,
}

/// Cumulative host/worker timing for the opt-in threaded DSP link profiler.
/// Durations are nanoseconds and overlap across pipelined frames, so they are
/// explanatory stage totals rather than additive wall time.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadedTimingProfile {
    pub frames_sent: u64,
    pub frames_completed: u64,
    pub host_enqueue_ns: u64,
    pub host_reply_wait_ns: u64,
    pub host_done_wait_ns: u64,
    pub host_collect_ns: u64,
    pub worker_queue_ns: u64,
    pub worker_spi_ns: u64,
    pub worker_dsp_ns: u64,
    pub worker_sport_ns: u64,
}

impl ThreadedTimingProfile {
    fn add_worker(&mut self, timing: WorkerTiming) {
        self.frames_completed = self.frames_completed.saturating_add(1);
        self.worker_queue_ns = self.worker_queue_ns.saturating_add(timing.queue_ns);
        self.worker_spi_ns = self.worker_spi_ns.saturating_add(timing.spi_ns);
        self.worker_dsp_ns = self.worker_dsp_ns.saturating_add(timing.dsp_ns);
        self.worker_sport_ns = self.worker_sport_ns.saturating_add(timing.sport_ns);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerTiming {
    queue_ns: u64,
    spi_ns: u64,
    dsp_ns: u64,
    sport_ns: u64,
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

#[derive(Default)]
pub struct Shared {
    /// L/R interleaved, `-(q / 2^31)`.
    pub pcm: Vec<f32>,
    /// Raw Q31 words as the engine returned them.
    pub raw: Vec<i32>,
    pub frames: Vec<FrameStat>,
    pub first_words: BTreeMap<u16, u64>,
    pub halted: Option<String>,
    pub dsp_instructions: u64,
    pub nonzero_replies: u64,
    pub missing_blocks: u64,
    /// While false, frames get zero replies and the DSP does not run.
    pub attached: bool,
}

/// Everything about one frame that depends on the DSP engine. It is the
/// same code in the synchronous peer and in the DSP worker thread, so the
/// two cannot drift apart. It never touches [`Shared`].
struct Core<E: DspEngine> {
    engine: E,
    period: u32,
    idle: (u32, u32),
    /// Mirror of `Shared::halted` (only the core ever sets it).
    halted: Option<String>,
}

/// What the exchange half of a frame leaves for the render half.
struct Exchanged {
    stat: FrameStat,
    /// Detached or halted: zero reply, zero audio, no DSP time.
    skipped: bool,
}

/// What one frame adds to [`Shared`] once it has been rendered.
struct Done {
    stat: FrameStat,
    skipped: bool,
    block: Option<Vec<u8>>,
    halted: Option<String>,
    timing: Option<WorkerTiming>,
}

impl<E: DspEngine> Core<E> {
    fn new(engine: E, period: u32) -> Self {
        Self {
            engine,
            period,
            idle: DN2_IDLE_RANGE,
            halted: None,
        }
    }

    /// Run the DSP for one period; returns (executed, busy, reached_idle).
    fn run_period(&mut self) -> (u32, u32, bool) {
        let mut done = 0u32;
        let mut busy = None;
        let in_idle = |pc: u32, idle: (u32, u32)| (idle.0..idle.1).contains(&pc);
        // `busy` is the first chunk end (a multiple of CHUNK, or the period
        // end) with the PC in the idle range. No chunk end before the PC
        // first enters the range can be one, so run to its first entry at
        // once; if that is not a chunk end, run to the first chunk end with
        // the PC in the range (the engine looks at the same ends, without
        // cutting the generated blocks at the ones before). Engines without
        // these stops are stepped in chunks below.
        let base = self.engine.icount();
        if let Some(ran) = self.engine.step_until_in(self.period, self.idle) {
            done = ran;
            if !in_idle(self.engine.pc(), self.idle) {
                // The period ended (or the engine stopped) first.
                return (done, done, false);
            }
            if (done > 0 && done % CHUNK == 0) || done == self.period {
                busy = Some(done);
            } else if let Some(more) =
                self.engine
                    .step_until_in_aligned(self.period - done, self.idle, CHUNK, base)
            {
                // Stopped at a chunk end or the period end in the range
                // (busy, as in the chunk loop, even if the engine stopped
                // there), or elsewhere out of it (not busy).
                done += more;
                if in_idle(self.engine.pc(), self.idle) {
                    busy = Some(done);
                }
            }
        }
        while done < self.period {
            // Once idle is reached the chunking only costs time (the idle
            // skip is per step call); results do not depend on step sizes.
            let n = if busy.is_some() {
                self.period - done
            } else {
                ((done / CHUNK + 1) * CHUNK).min(self.period) - done
            };
            let ran = self.engine.step(n);
            done += ran;
            if busy.is_none() && in_idle(self.engine.pc(), self.idle) {
                busy = Some(done);
            }
            if ran < n {
                break;
            }
        }
        (done, busy.unwrap_or(done), busy.is_some())
    }

    /// First half: deliver the frame through the SPI2 model, take the reply.
    fn exchange(
        &mut self,
        tx: &[u8],
        attached: bool,
        timing: Option<&mut WorkerTiming>,
    ) -> (Vec<u8>, Exchanged) {
        let first_word = tx.get(..2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
        let mut stat = FrameStat {
            first_word,
            tx_hash: fnv1a(tx),
            ..FrameStat::default()
        };
        if !attached || self.halted.is_some() {
            return (
                vec![0; tx.len()],
                Exchanged {
                    stat,
                    skipped: true,
                },
            );
        }
        let spi_start = timing.as_ref().map(|_| Instant::now());
        let exchange = self.engine.spi2_exchange(tx);
        if let (Some(timing), Some(start)) = (timing, spi_start) {
            timing.spi_ns = timing.spi_ns.saturating_add(elapsed_ns(start));
        }
        let reply = match exchange {
            Ok(r) if r.len() == tx.len() => r,
            Ok(r) => {
                eprintln!("sharc_peer: reply length {} != {}", r.len(), tx.len());
                self.halted = Some("bad reply length".into());
                vec![0; tx.len()]
            }
            Err(e) => {
                eprintln!("sharc_peer: spi2_exchange failed: {e}");
                self.halted = Some(e);
                vec![0; tx.len()]
            }
        };
        stat.reply_hash = fnv1a(&reply);
        stat.reply_nonzero_bytes = reply.iter().filter(|&&b| b != 0).count() as u32;
        (
            reply,
            Exchanged {
                stat,
                skipped: false,
            },
        )
    }

    /// Second half: run the DSP for the period and take the audio block.
    /// Reads only the engine state left by the exchange.
    fn render(&mut self, ex: Exchanged, mut timing: Option<&mut WorkerTiming>) -> Done {
        let Exchanged { mut stat, skipped } = ex;
        if skipped {
            return Done {
                stat,
                skipped,
                block: None,
                halted: self.halted.clone(),
                timing: timing.copied(),
            };
        }
        let mut halted = self.halted.clone();
        if halted.is_none() {
            let dsp_start = timing.as_ref().map(|_| Instant::now());
            let (executed, busy, idle) = self.run_period();
            if let (Some(timing), Some(start)) = (timing.as_deref_mut(), dsp_start) {
                timing.dsp_ns = timing.dsp_ns.saturating_add(elapsed_ns(start));
            }
            stat.executed = executed;
            stat.busy = busy;
            stat.reached_idle = idle;
            halted = self.engine.halt_reason();
            if halted.is_some() {
                eprintln!(
                    "sharc_peer: DSP halted: {}",
                    halted.as_deref().unwrap_or("")
                );
            }
        }
        let block = if halted.is_none() {
            let sport_start = timing.as_ref().map(|_| Instant::now());
            let block = self.engine.sport_block();
            if let (Some(timing), Some(start)) = (timing.as_deref_mut(), sport_start) {
                timing.sport_ns = timing.sport_ns.saturating_add(elapsed_ns(start));
            }
            match block {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("sharc_peer: sport_block failed: {e}");
                    halted = Some(e);
                    None
                }
            }
        } else {
            None
        };
        if halted.is_some() && self.halted.is_none() {
            self.halted = halted;
        }
        Done {
            stat,
            skipped,
            block,
            halted: self.halted.clone(),
            timing: timing.copied(),
        }
    }
}

/// Append one finished frame to the shared record.
fn commit(sh: &mut Shared, d: Done) {
    let Done {
        stat,
        skipped,
        block,
        halted,
        timing: _,
    } = d;
    *sh.first_words.entry(stat.first_word).or_default() += 1;
    if skipped {
        sh.pcm.extend_from_slice(&[0.0; BLOCK_WORDS]);
        sh.raw.extend_from_slice(&[0; BLOCK_WORDS]);
        sh.frames.push(stat);
        return;
    }
    let mut stat = stat;
    sh.nonzero_replies += (stat.reply_nonzero_bytes != 0) as u64;
    sh.dsp_instructions += stat.executed as u64;
    if halted.is_some() && sh.halted.is_none() {
        sh.halted = halted;
    }
    match block {
        Some(b) if b.len() == BLOCK_WORDS * 4 => {
            stat.block = true;
            for w in b.chunks_exact(4) {
                let q = i32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                sh.raw.push(q);
                sh.pcm.push(-(q as f64 / 2_147_483_648.0) as f32);
            }
        }
        _ => {
            sh.missing_blocks += 1;
            sh.raw.extend_from_slice(&[0; BLOCK_WORDS]);
            sh.pcm.extend_from_slice(&[0.0; BLOCK_WORDS]);
        }
    }
    sh.frames.push(stat);
}

/// The synchronous peer: the whole frame runs inside the ColdFire bus write.
pub struct SharcPeer<E: DspEngine> {
    core: Core<E>,
    shared: Rc<RefCell<Shared>>,
}

impl<E: DspEngine> SharcPeer<E> {
    pub fn new(engine: E, period: u32) -> (Self, Rc<RefCell<Shared>>) {
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let peer = Self {
            core: Core::new(engine, period),
            shared: shared.clone(),
        };
        (peer, shared)
    }

    pub fn set_idle_range(&mut self, range: (u32, u32)) {
        self.core.idle = range;
    }

    fn frame(&mut self, tx: &[u8]) -> Vec<u8> {
        let attached = self.shared.borrow().attached;
        let (reply, ex) = self.core.exchange(tx, attached, None);
        let done = self.core.render(ex, None);
        commit(&mut self.shared.borrow_mut(), done);
        reply
    }
}

impl<E: DspEngine + 'static> SharcPeer<E> {
    pub fn boxed(self) -> Box<dyn Peer> {
        Box::new(self)
    }
}

impl<E: DspEngine> Peer for SharcPeer<E> {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        self.frame(tx)
    }
}

enum Msg {
    Frame {
        tx: Vec<u8>,
        attached: bool,
        enqueued: Option<Instant>,
    },
    Export,
}

/// ColdFire-thread side of the threaded peer.
struct Link {
    tx: Option<mpsc::Sender<Msg>>,
    reply_rx: mpsc::Receiver<Vec<u8>>,
    done_rx: mpsc::Receiver<Done>,
    export_rx: mpsc::Receiver<(Vec<u8>, u64)>,
    /// Frames sent whose [`Done`] has not been committed yet.
    outstanding: usize,
    timing: Option<ThreadedTimingProfile>,
    shared: Rc<RefCell<Shared>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Link {
    fn commit_ready(&mut self, block: bool) {
        while self.outstanding > 0 {
            let done_wait_start = if block && self.timing.is_some() {
                Some(Instant::now())
            } else {
                None
            };
            let done = if block {
                self.done_rx.recv().ok()
            } else {
                self.done_rx.try_recv().ok()
            };
            if let (Some(profile), Some(start)) = (&mut self.timing, done_wait_start) {
                profile.host_done_wait_ns =
                    profile.host_done_wait_ns.saturating_add(elapsed_ns(start));
            }
            let Some(d) = done else { break };
            let collect_start = self.timing.as_ref().map(|_| Instant::now());
            let worker_timing = d.timing;
            commit(&mut self.shared.borrow_mut(), d);
            if let Some(profile) = &mut self.timing {
                if let Some(timing) = worker_timing {
                    profile.add_worker(timing);
                }
                if let Some(start) = collect_start {
                    profile.host_collect_ns =
                        profile.host_collect_ns.saturating_add(elapsed_ns(start));
                }
            }
            self.outstanding -= 1;
        }
    }

    fn worker_died(&mut self, len: usize) -> Vec<u8> {
        let mut sh = self.shared.borrow_mut();
        if sh.halted.is_none() {
            sh.halted = Some("DSP worker thread stopped".into());
        }
        sh.attached = false;
        vec![0; len]
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.tx = None; // the worker's recv fails and the thread ends
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Threaded peer. Frame N: the ColdFire thread sends the frame to the DSP
/// worker and waits only for the reply, which the worker produces after it
/// has finished rendering frame N-1 (one thread, in order), so the reply and
/// every DSP state it depends on are exactly those of the synchronous peer.
/// The worker then renders period N while the ColdFire thread continues to
/// frame N+1. [`Shared`] receives each frame's PCM and statistics in order,
/// when the ColdFire thread next looks (`exchange`, [`ThreadedHandle::sync`]);
/// the DSP engine is created inside the worker thread, so it need not be
/// `Send`.
pub struct ThreadedPeer {
    link: Rc<RefCell<Link>>,
}

/// Host-side handle to a [`ThreadedPeer`] that the ColdFire emulator owns.
#[derive(Clone)]
pub struct ThreadedHandle {
    link: Rc<RefCell<Link>>,
}

impl ThreadedPeer {
    /// Start the worker. `make` runs on the worker thread and builds the
    /// engine there (an error is returned here).
    pub fn spawn<E, F>(
        make: F,
        period: u32,
        idle: (u32, u32),
    ) -> Result<(Self, ThreadedHandle, Rc<RefCell<Shared>>), String>
    where
        E: DspEngine + 'static,
        F: FnOnce() -> Result<E, String> + Send + 'static,
    {
        Self::spawn_with_timing(make, period, idle, false)
    }

    /// Start the worker with optional cumulative host/worker timing.
    pub fn spawn_with_timing<E, F>(
        make: F,
        period: u32,
        idle: (u32, u32),
        timing_enabled: bool,
    ) -> Result<(Self, ThreadedHandle, Rc<RefCell<Shared>>), String>
    where
        E: DspEngine + 'static,
        F: FnOnce() -> Result<E, String> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (reply_tx, reply_rx) = mpsc::channel::<Vec<u8>>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let (export_tx, export_rx) = mpsc::channel::<(Vec<u8>, u64)>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let worker = thread::Builder::new()
            .name("sharc-dsp".into())
            .spawn(move || {
                let engine = match make() {
                    Ok(e) => {
                        let _ = ready_tx.send(Ok(()));
                        e
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let mut core = Core::new(engine, period);
                core.idle = idle;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        Msg::Frame {
                            tx,
                            attached,
                            enqueued,
                        } => {
                            let mut timing = enqueued.map(|start| WorkerTiming {
                                queue_ns: elapsed_ns(start),
                                ..WorkerTiming::default()
                            });
                            let (reply, ex) = core.exchange(&tx, attached, timing.as_mut());
                            if reply_tx.send(reply).is_err() {
                                return;
                            }
                            if done_tx.send(core.render(ex, timing.as_mut())).is_err() {
                                return;
                            }
                        }
                        Msg::Export => {
                            let _ = export_tx.send((core.engine.export(), core.engine.icount()));
                        }
                    }
                }
            })
            .map_err(|e| format!("spawn DSP worker: {e}"))?;
        ready_rx
            .recv()
            .map_err(|_| "DSP worker died at start".to_string())??;
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let link = Rc::new(RefCell::new(Link {
            tx: Some(tx),
            reply_rx,
            done_rx,
            export_rx,
            outstanding: 0,
            timing: timing_enabled.then(ThreadedTimingProfile::default),
            shared: shared.clone(),
            worker: Some(worker),
        }));
        Ok((Self { link: link.clone() }, ThreadedHandle { link }, shared))
    }

    pub fn boxed(self) -> Box<dyn Peer> {
        Box::new(self)
    }
}

impl Peer for ThreadedPeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut l = self.link.borrow_mut();
        let attached = l.shared.borrow().attached;
        let enqueue_start = l.timing.as_ref().map(|_| Instant::now());
        let tx = tx.to_vec();
        let tx_len = tx.len();
        let enqueued = l.timing.as_ref().map(|_| Instant::now());
        let sent = l.tx.as_ref().is_some_and(|c| {
            c.send(Msg::Frame {
                tx,
                attached,
                enqueued,
            })
            .is_ok()
        });
        if let (Some(profile), Some(start)) = (&mut l.timing, enqueue_start) {
            profile.host_enqueue_ns = profile.host_enqueue_ns.saturating_add(elapsed_ns(start));
        }
        if !sent {
            return l.worker_died(tx_len);
        }
        l.outstanding += 1;
        if let Some(profile) = &mut l.timing {
            profile.frames_sent = profile.frames_sent.saturating_add(1);
        }
        let reply_wait_start = l.timing.as_ref().map(|_| Instant::now());
        match l.reply_rx.recv() {
            Ok(r) => {
                if let (Some(profile), Some(start)) = (&mut l.timing, reply_wait_start) {
                    profile.host_reply_wait_ns =
                        profile.host_reply_wait_ns.saturating_add(elapsed_ns(start));
                }
                l.commit_ready(false);
                r
            }
            Err(_) => {
                if let (Some(profile), Some(start)) = (&mut l.timing, reply_wait_start) {
                    profile.host_reply_wait_ns =
                        profile.host_reply_wait_ns.saturating_add(elapsed_ns(start));
                }
                l.worker_died(tx_len)
            }
        }
    }
}

impl ThreadedHandle {
    /// Commit completed frames without waiting; streaming hosts use this between steps.
    /// Call `sync` instead before reading final totals or exporting state.
    pub fn poll(&self) {
        self.link.borrow_mut().commit_ready(false);
    }

    /// Wait for the DSP to finish every frame sent so far and commit them to
    /// [`Shared`]. Call before reading `Shared` for totals or a snapshot.
    pub fn sync(&self) {
        self.link.borrow_mut().commit_ready(true);
    }

    /// Cumulative opt-in timing, or `None` when the peer was started without it.
    pub fn timing_profile(&self) -> Option<ThreadedTimingProfile> {
        self.link.borrow().timing
    }

    /// Canonical DSP state and instruction counter after every frame sent so
    /// far (empty / 0 when the engine does not export).
    pub fn export(&self) -> (Vec<u8>, u64) {
        let mut l = self.link.borrow_mut();
        l.commit_ready(true);
        let sent = l.tx.as_ref().is_some_and(|c| c.send(Msg::Export).is_ok());
        if !sent {
            return (Vec::new(), 0);
        }
        l.export_rx.recv().unwrap_or_default()
    }
}

/// The native engine as a [`DspEngine`].
pub struct NativeDsp(pub sharc_native::Engine);

impl DspEngine for NativeDsp {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        self.0
            .spi2_exchange(frame)
            .map_err(|t| format!("spi2_exchange trap {}", sharc_native::trap_name(t)))
    }
    fn step(&mut self, n: u32) -> u32 {
        self.0.step(n)
    }
    fn step_until_in(&mut self, n: u32, range: (u32, u32)) -> Option<u32> {
        Some(self.0.step_until_in(n, range.0, range.1))
    }
    fn step_until_in_aligned(
        &mut self,
        n: u32,
        range: (u32, u32),
        chunk: u32,
        base_icount: u64,
    ) -> Option<u32> {
        Some(
            self.0
                .step_until_in_aligned(n, range.0, range.1, chunk, base_icount),
        )
    }
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .sport_block(None)
            .map_err(|t| format!("sport_block trap {}", sharc_native::trap_name(t)))
    }
    fn pc(&self) -> u32 {
        self.0.s.pc_sw as u32
    }
    fn halt_reason(&self) -> Option<String> {
        self.0.halt.clone()
    }
    fn export(&self) -> Vec<u8> {
        self.0.export()
    }
    fn icount(&self) -> u64 {
        self.0.s.icount
    }
}

/// A native engine that the host keeps a handle to, so it can export the
/// canonical state (coupled snapshots) while the peer owns the engine.
#[derive(Clone)]
pub struct SharedDsp(pub Rc<RefCell<sharc_native::Engine>>);

impl SharedDsp {
    pub fn new(e: sharc_native::Engine) -> Self {
        Self(Rc::new(RefCell::new(e)))
    }
    /// Canonical state blob of the engine right now.
    pub fn export(&self) -> Vec<u8> {
        self.0.borrow().export()
    }
}

impl DspEngine for SharedDsp {
    fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        self.0
            .borrow_mut()
            .spi2_exchange(frame)
            .map_err(|t| format!("spi2_exchange trap {}", sharc_native::trap_name(t)))
    }
    fn step(&mut self, n: u32) -> u32 {
        self.0.borrow_mut().step(n)
    }
    fn step_until_in(&mut self, n: u32, range: (u32, u32)) -> Option<u32> {
        Some(self.0.borrow_mut().step_until_in(n, range.0, range.1))
    }
    fn step_until_in_aligned(
        &mut self,
        n: u32,
        range: (u32, u32),
        chunk: u32,
        base_icount: u64,
    ) -> Option<u32> {
        Some(
            self.0
                .borrow_mut()
                .step_until_in_aligned(n, range.0, range.1, chunk, base_icount),
        )
    }
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .borrow_mut()
            .sport_block(None)
            .map_err(|t| format!("sport_block trap {}", sharc_native::trap_name(t)))
    }
    fn pc(&self) -> u32 {
        self.0.borrow().s.pc_sw as u32
    }
    fn halt_reason(&self) -> Option<String> {
        self.0.borrow().halt.clone()
    }
    fn export(&self) -> Vec<u8> {
        self.0.borrow().export()
    }
    fn icount(&self) -> u64 {
        self.0.borrow().s.icount
    }
}

/// Build the DN2 continuation engine from a packed image blob
/// (`tools/sharc_pack_image.py`) and a canonical state blob, with the option
/// set the audio runs use. `clock_base` seeds the diagnostic instruction
/// clock (EMUCLK).
pub fn open_dn2_engine(
    image: &[u8],
    state: &[u8],
    clock_base: u64,
) -> Result<sharc_native::Engine, String> {
    let mut e = sharc_native::Engine::from_image(image).map_err(|c| format!("image error {c}"))?;
    e.import(state)
        .map_err(|c| format!("state import error {c}"))?;
    // 4 runtime decode, 5 instruction clock, 6 clock base, 9 software IRQs,
    // 10 explicit memory model off, 11 approx recips, 12 assume NW32,
    // 21 core timer, 22 peripheral model (19/20 come from the state).
    let mut options = vec![
        (4, 1),
        (5, 1),
        (6, clock_base as i64),
        (9, 1),
        (10, 0),
        (11, 1),
        (12, 1),
        (21, 1),
        (22, 1),
    ];
    // 23-25 idle-loop skip (exact: see docs/findings/07-emulator.md);
    // DSP_IDLE_SKIP=0 turns it off.
    if std::env::var("DSP_IDLE_SKIP").as_deref() != Ok("0") {
        options.extend([
            (23, DN2_IDLE_HEAD as i64),
            (24, DN2_IDLE_RANGE.0 as i64),
            (25, DN2_IDLE_RANGE.1 as i64),
        ]);
    }
    for (k, v) in options {
        if e.set_option(k, v) != 0 {
            return Err(format!("set_option {k} rejected"));
        }
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        pc: u32,
        idle_after: u32,
        ran: u32,
        halt_at_frame: Option<u32>,
        frames: u32,
        reply_byte: u8,
    }

    impl DspEngine for Mock {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            self.frames += 1;
            self.ran = 0;
            Ok(vec![self.reply_byte; f.len()])
        }
        fn step(&mut self, n: u32) -> u32 {
            self.ran += n;
            self.pc = if self.ran >= self.idle_after {
                DN2_IDLE_RANGE.0 + 3
            } else {
                0x1c0000
            };
            n
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.halt_at_frame == Some(self.frames) {
                return Err("boom".into());
            }
            let mut b = Vec::new();
            for i in 0..BLOCK_WORDS as i32 {
                // L = -(2^30), R = 2^30 on every frame.
                let w: i32 = if i % 2 == 0 { -(1 << 30) } else { 1 << 30 };
                b.extend_from_slice(&w.to_le_bytes());
            }
            Ok(Some(b))
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn halt_reason(&self) -> Option<String> {
            None
        }
    }

    fn mock(halt_at_frame: Option<u32>) -> Mock {
        Mock {
            pc: 0,
            idle_after: 10_000,
            ran: 0,
            halt_at_frame,
            frames: 0,
            reply_byte: 0xab,
        }
    }

    /// A PC that is in the idle range exactly while the instruction count
    /// lies in one of `idle` ([start, end) intervals), optionally able to
    /// stop at the first such boundary (`fast`), and halting at `halt`.
    struct Spans {
        t: u32,
        idle: Vec<(u32, u32)>,
        halt: Option<u32>,
        fast: bool,
    }

    impl Spans {
        fn idle_at(&self, t: u32) -> bool {
            self.idle.iter().any(|&(a, b)| (a..b).contains(&t))
        }
        fn run(&mut self, n: u32, stop_in_idle: bool) -> u32 {
            let start = self.t;
            for _ in 0..n {
                if stop_in_idle && self.idle_at(self.t) {
                    break;
                }
                if self.halt == Some(self.t) {
                    break;
                }
                self.t += 1;
            }
            self.t - start
        }
    }

    impl DspEngine for Spans {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            Ok(vec![0; f.len()])
        }
        fn step(&mut self, n: u32) -> u32 {
            self.run(n, false)
        }
        fn step_until_in(&mut self, n: u32, _range: (u32, u32)) -> Option<u32> {
            self.fast.then(|| self.run(n, true))
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn pc(&self) -> u32 {
            if self.idle_at(self.t) {
                DN2_IDLE_RANGE.0
            } else {
                0x1c0000
            }
        }
        fn halt_reason(&self) -> Option<String> {
            (self.halt == Some(self.t)).then(|| "halt".into())
        }
    }

    #[test]
    fn stopping_at_idle_entry_finds_the_same_chunk_end() {
        let period = 10_000;
        let cases: Vec<(Vec<(u32, u32)>, Option<u32>)> = vec![
            (vec![], None),
            (vec![(0, period)], None),
            (vec![(2048, period)], None),
            (vec![(2047, period)], None),
            (vec![(2049, period)], None),
            (vec![(3000, 3100), (5000, period)], None),
            (vec![(3000, 3072), (3073, period)], None),
            (vec![(3000, 3073), (3080, period)], None),
            (vec![(9999, period)], None),
            (vec![(period - 10, period + 1)], None),
            (vec![(5000, period)], Some(4000)),
            (vec![(5000, period)], Some(5500)),
            (vec![(1000, 1500), (6000, period)], Some(7000)),
        ];
        for (idle, halt) in cases {
            let mut got = Vec::new();
            for fast in [false, true] {
                let mut core = Core::new(
                    Spans {
                        t: 0,
                        idle: idle.clone(),
                        halt,
                        fast,
                    },
                    period,
                );
                got.push(core.run_period());
            }
            assert_eq!(got[0], got[1], "idle {idle:?} halt {halt:?}");
        }
    }

    #[test]
    fn frame_flow_pcm_and_busy() {
        let (mut peer, sh) = SharcPeer::new(mock(None), 20_000);
        let mut tx = vec![0u8; 2748];
        tx[1] = 3;
        let reply = peer.exchange(&tx);
        assert_eq!(reply.len(), 2748);
        assert!(reply.iter().all(|&b| b == 0xab));
        let sh = sh.borrow();
        assert_eq!(sh.pcm.len(), BLOCK_WORDS);
        assert_eq!(sh.pcm[0], 0.5);
        assert_eq!(sh.pcm[1], -0.5);
        assert_eq!(sh.raw[0], -(1 << 30));
        let f = &sh.frames[0];
        assert_eq!(f.first_word, 3);
        assert_eq!(f.reply_nonzero_bytes, 2748);
        assert!(f.reached_idle);
        // first 1024-instruction chunk ending at or past 10,000
        assert_eq!(f.busy, 10 * 1024);
        assert_eq!(f.executed, 20_000);
        assert_eq!(sh.dsp_instructions, 20_000);
        assert_eq!(sh.first_words[&3], 1);
    }

    #[test]
    fn engine_error_returns_zeros_and_stays_halted() {
        let (mut peer, sh) = SharcPeer::new(mock(Some(1)), 4096);
        let r = peer.exchange(&[0u8; 8]);
        assert_eq!(r, vec![0xab; 8]); // reply was taken before the block failed
        assert_eq!(sh.borrow().halted.as_deref(), Some("boom"));
        let r = peer.exchange(&[0u8; 8]);
        assert_eq!(r, vec![0; 8]);
        let sh = sh.borrow();
        assert_eq!(sh.frames.len(), 2);
        assert_eq!(sh.pcm.len(), 2 * BLOCK_WORDS);
        assert!(sh.pcm[BLOCK_WORDS..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn detached_gives_zero_reply_without_running() {
        let (mut peer, sh) = SharcPeer::new(mock(None), 4096);
        sh.borrow_mut().attached = false;
        assert_eq!(peer.exchange(&[1u8; 4]), vec![0; 4]);
        assert_eq!(sh.borrow().dsp_instructions, 0);
        assert_eq!(peer.core.engine.frames, 0);
    }
}

#[cfg(test)]
mod threaded_tests {
    use super::*;

    /// State-dependent engine: every reply and block depends on everything
    /// that happened before, so any reordering shows up in the output.
    struct Chain {
        h: u64,
        halt_after: Option<u32>,
        frames: u32,
        pc: u32,
    }

    impl Chain {
        fn new(halt_after: Option<u32>) -> Self {
            Chain {
                h: 0x1234_5678,
                halt_after,
                frames: 0,
                pc: 0,
            }
        }
        fn mix(&mut self, v: u64) {
            self.h = (self.h ^ v).wrapping_mul(0x100_0000_01b3).rotate_left(13);
        }
    }

    impl DspEngine for Chain {
        fn spi2_exchange(&mut self, f: &[u8]) -> Result<Vec<u8>, String> {
            self.frames += 1;
            self.mix(fnv1a(f));
            let h = self.h;
            Ok((0..f.len()).map(|i| (h >> ((i % 8) * 8)) as u8).collect())
        }
        fn step(&mut self, n: u32) -> u32 {
            self.mix(n as u64);
            self.pc = if self.h & 3 == 0 {
                DN2_IDLE_RANGE.0 + 1
            } else {
                0x1c0000
            };
            n
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.halt_after == Some(self.frames) {
                return Err("chain halt".into());
            }
            self.mix(7);
            let mut b = Vec::new();
            for i in 0..BLOCK_WORDS as u64 {
                self.mix(i);
                b.extend_from_slice(&((self.h >> 20) as i32).to_le_bytes());
            }
            Ok(Some(b))
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn halt_reason(&self) -> Option<String> {
            None
        }
        fn export(&self) -> Vec<u8> {
            self.h.to_le_bytes().to_vec()
        }
        fn icount(&self) -> u64 {
            self.frames as u64
        }
    }

    fn tx_for(i: u32) -> Vec<u8> {
        let mut tx = vec![0u8; 64];
        tx[0..2].copy_from_slice(&((i % 5) as u16).to_be_bytes());
        tx[8..12].copy_from_slice(&i.to_le_bytes());
        tx
    }

    type Stat = (u16, u64, u64, u32, u32, bool, u32, bool);
    /// (replies, pcm bits, raw, per-frame stats, counters, halted)
    type Out = (
        Vec<Vec<u8>>,
        Vec<u32>,
        Vec<i32>,
        Vec<Stat>,
        (u64, u64, u64),
        Option<String>,
    );

    fn collect(sh: &Shared, replies: Vec<Vec<u8>>) -> Out {
        (
            replies,
            sh.pcm.iter().map(|x| x.to_bits()).collect(),
            sh.raw.clone(),
            sh.frames
                .iter()
                .map(|f| {
                    (
                        f.first_word,
                        f.tx_hash,
                        f.reply_hash,
                        f.reply_nonzero_bytes,
                        f.busy,
                        f.reached_idle,
                        f.executed,
                        f.block,
                    )
                })
                .collect(),
            (sh.dsp_instructions, sh.nonzero_replies, sh.missing_blocks),
            sh.halted.clone(),
        )
    }

    /// Frames 40..60 are sent detached.
    fn script(i: u32) -> bool {
        !(40..60).contains(&i)
    }

    fn run_sync(n: u32, halt: Option<u32>) -> Out {
        let (mut peer, sh) = SharcPeer::new(Chain::new(halt), 5000);
        let mut replies = Vec::new();
        for i in 0..n {
            sh.borrow_mut().attached = script(i);
            replies.push(peer.exchange(&tx_for(i)));
        }
        let sh = sh.borrow();
        collect(&sh, replies)
    }

    fn run_threaded(n: u32, halt: Option<u32>, poll: bool) -> (Out, (Vec<u8>, u64)) {
        let (mut peer, handle, sh) =
            ThreadedPeer::spawn(move || Ok(Chain::new(halt)), 5000, DN2_IDLE_RANGE).unwrap();
        let mut replies = Vec::new();
        for i in 0..n {
            sh.borrow_mut().attached = script(i);
            replies.push(peer.exchange(&tx_for(i)));
            if poll && i % 17 == 0 {
                handle.sync();
            }
        }
        let exp = handle.export();
        let sh = sh.borrow();
        (collect(&sh, replies), exp)
    }

    #[test]
    fn threaded_matches_sync() {
        for halt in [None, Some(150)] {
            let want = run_sync(300, halt);
            for poll in [false, true] {
                let (got, exp) = run_threaded(300, halt, poll);
                assert!(got == want, "halt={halt:?} poll={poll}");
                assert_eq!(exp.0.len(), 8);
                assert_eq!(exp.1, if halt.is_some() { 150 } else { 280 });
            }
        }
        // The script really exercised the detach and halt paths.
        let w = run_sync(300, Some(150));
        assert_eq!(w.5.as_deref(), Some("chain halt"));
        assert!(w.3[45].2 == 0 && !w.3[45].7);
        assert!(w.3[100].7);
    }

    #[test]
    fn threaded_engine_start_error_is_reported() {
        let r = ThreadedPeer::spawn(
            || Err::<Chain, _>("no engine".to_string()),
            1,
            DN2_IDLE_RANGE,
        );
        assert_eq!(r.err().as_deref(), Some("no engine"));
    }
}

/// `Core::run_period` against the plain chunked loop on the real DN2 engine
/// (private fixtures; run with `--ignored`, see the test).
#[cfg(test)]
mod aligned_stop_tests {
    use super::*;

    /// The native engine with only `step`: `run_period` falls back to the
    /// chunk loop alone, the reference the stops must not differ from.
    struct ChunkedOnly(NativeDsp);

    impl DspEngine for ChunkedOnly {
        fn spi2_exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
            self.0.spi2_exchange(frame)
        }
        fn step(&mut self, n: u32) -> u32 {
            self.0.step(n)
        }
        fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            self.0.sport_block()
        }
        fn pc(&self) -> u32 {
            self.0.pc()
        }
        fn halt_reason(&self) -> Option<String> {
            self.0.halt_reason()
        }
        fn export(&self) -> Vec<u8> {
            self.0.export()
        }
        fn icount(&self) -> u64 {
            self.0.icount()
        }
    }

    fn read_frames(path: &str) -> Vec<Vec<u8>> {
        let data = std::fs::read(path).expect("frames file");
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        let mut o = 4;
        for _ in 0..u32_at(0) {
            let len = u32_at(o);
            out.push(data[o + 4..o + 4 + len].to_vec());
            o += 4 + len;
        }
        out
    }

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
    }

    /// DN2_EQUIV_IMAGE (packed image), DN2_EQUIV_STATE (canonical state the
    /// frames START continues), DN2_EQUIV_FRAMES (`u32 count`, then `u32
    /// length, bytes` per DSPI2 TX frame); optional DN2_EQUIV_START /
    /// DN2_EQUIV_END (default 4600 / 5600) and DN2_EQUIV_CLOCK. Every period
    /// must return the same (executed, busy, reached_idle) and the engines
    /// must end in the same state.
    #[test]
    #[ignore = "needs the DN2 replay fixtures"]
    fn run_period_matches_chunk_loop() {
        let image = std::fs::read(env("DN2_EQUIV_IMAGE")).unwrap();
        let state = std::fs::read(env("DN2_EQUIV_STATE")).unwrap();
        let frames = read_frames(&env("DN2_EQUIV_FRAMES"));
        let num = |k: &str, d: usize| std::env::var(k).map_or(d, |v| v.parse().unwrap());
        let (start, end) = (num("DN2_EQUIV_START", 4600), num("DN2_EQUIV_END", 5600));
        let clock = num("DN2_EQUIV_CLOCK", 573_627_620) as u64;
        let period = 667_000;
        let open = || NativeDsp(open_dn2_engine(&image, &state, clock).unwrap());
        let mut new = Core::new(open(), period);
        let mut old = Core::new(ChunkedOnly(open()), period);
        let (mut busy_before_end, mut nonidle) = (0, 0);
        for (i, frame) in frames.iter().enumerate().take(end).skip(start) {
            let (r_new, _) = new.exchange(frame, true, None);
            let (r_old, _) = old.exchange(frame, true, None);
            assert_eq!(r_new, r_old, "SPI2 reply, frame {i}");
            let got = new.run_period();
            let want = old.run_period();
            assert_eq!(got, want, "(executed, busy, reached_idle), frame {i}");
            busy_before_end += (want.2 && want.1 < want.0) as u32;
            nonidle += !want.2 as u32;
            assert_eq!(
                new.engine.sport_block().unwrap(),
                old.engine.sport_block().unwrap(),
                "SPORT4 block, frame {i}"
            );
        }
        assert!(new.engine.halt_reason().is_none() && old.engine.halt_reason().is_none());
        new.engine.0.export_ranges = true;
        old.engine.0.0.export_ranges = true;
        let (a, b) = (new.engine.export(), old.engine.export());
        assert!(a == b, "final state differs");
        eprintln!(
            "{} periods equal; idle reached before the period end in {busy_before_end}, idle never reached in \
             {nonidle}; state fnv {:016x} ({} bytes)",
            end - start,
            fnv1a(&a),
            a.len()
        );
        assert!(busy_before_end > 0, "the idle stops were never exercised");
    }
}
