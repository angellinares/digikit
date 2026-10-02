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

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

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
    /// One SPORT4 block (zero input); `Ok(None)` while the SPORTs are off.
    fn sport_block(&mut self) -> Result<Option<Vec<u8>>, String>;
    /// Short-word PC.
    fn pc(&self) -> u32;
    /// Halt reason, once stopped.
    fn halt_reason(&self) -> Option<String>;
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

pub struct SharcPeer<E: DspEngine> {
    engine: E,
    period: u32,
    idle: (u32, u32),
    shared: Rc<RefCell<Shared>>,
}

impl<E: DspEngine> SharcPeer<E> {
    pub fn new(engine: E, period: u32) -> (Self, Rc<RefCell<Shared>>) {
        let shared = Rc::new(RefCell::new(Shared {
            attached: true,
            ..Shared::default()
        }));
        let peer = Self {
            engine,
            period,
            idle: DN2_IDLE_RANGE,
            shared: shared.clone(),
        };
        (peer, shared)
    }

    pub fn set_idle_range(&mut self, range: (u32, u32)) {
        self.idle = range;
    }

    /// Run the DSP for one period; returns (executed, busy, reached_idle).
    fn run_period(&mut self) -> (u32, u32, bool) {
        let mut done = 0u32;
        let mut busy = None;
        while done < self.period {
            // Once idle is reached the chunking only costs time (the idle
            // skip is per step call); results do not depend on step sizes.
            let n = if busy.is_some() {
                self.period - done
            } else {
                CHUNK.min(self.period - done)
            };
            let ran = self.engine.step(n);
            done += ran;
            if busy.is_none() && (self.idle.0..self.idle.1).contains(&self.engine.pc()) {
                busy = Some(done);
            }
            if ran < n {
                break;
            }
        }
        (done, busy.unwrap_or(done), busy.is_some())
    }

    fn frame(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut sh = self.shared.borrow_mut();
        let first_word = tx.get(..2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
        *sh.first_words.entry(first_word).or_default() += 1;
        let mut stat = FrameStat {
            first_word,
            tx_hash: fnv1a(tx),
            ..FrameStat::default()
        };
        if !sh.attached || sh.halted.is_some() {
            sh.pcm.extend_from_slice(&[0.0; BLOCK_WORDS]);
            sh.raw.extend_from_slice(&[0; BLOCK_WORDS]);
            sh.frames.push(stat);
            return vec![0; tx.len()];
        }
        let reply = match self.engine.spi2_exchange(tx) {
            Ok(r) if r.len() == tx.len() => r,
            Ok(r) => {
                eprintln!("sharc_peer: reply length {} != {}", r.len(), tx.len());
                sh.halted = Some("bad reply length".into());
                vec![0; tx.len()]
            }
            Err(e) => {
                eprintln!("sharc_peer: spi2_exchange failed: {e}");
                sh.halted = Some(e);
                vec![0; tx.len()]
            }
        };
        stat.reply_hash = fnv1a(&reply);
        stat.reply_nonzero_bytes = reply.iter().filter(|&&b| b != 0).count() as u32;
        sh.nonzero_replies += (stat.reply_nonzero_bytes != 0) as u64;
        drop(sh);
        let mut halted = self.shared.borrow().halted.clone();
        if halted.is_none() {
            let (executed, busy, idle) = self.run_period();
            stat.executed = executed;
            stat.busy = busy;
            stat.reached_idle = idle;
            self.shared.borrow_mut().dsp_instructions += executed as u64;
            halted = self.engine.halt_reason();
            if halted.is_some() {
                eprintln!(
                    "sharc_peer: DSP halted: {}",
                    halted.as_deref().unwrap_or("")
                );
            }
        }
        let block = if halted.is_none() {
            match self.engine.sport_block() {
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
        let mut sh = self.shared.borrow_mut();
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
        assert_eq!(peer.engine.frames, 0);
    }
}
