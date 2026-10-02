//! Headless live coupling: the native ColdFire emulator and the native SHARC+
//! engine run in lockstep over the DSPI2 link and produce audio.
//!
//! usage: sharc_live SYX DSP_STATE DSP_IMAGE OUT.wav [EXTRA_AFTER_READY=250000000]
//!   DSP_STATE  canonical SHRD state blob (a DN2 continuation; private)
//!   DSP_IMAGE  packed loader image: `uv run python tools/sharc_pack_image.py dn2-1.11 OUT`
//!   OUT.wav    48 kHz stereo 32-bit float; OUT.q31 raw words; OUT.csv per frame
//! env: NOTE_EVENTS=trig|play (see dspi2_capture), DSP_PERIOD (instructions
//!   per frame, default 666667), DSP_CLOCK_BASE (default 573627620),
//!   DSP_ATTACH=start|ready (default start: the DSP sees every frame from the
//!   first one; ready: zero replies and no DSP time until the panel is ready).
//!   CF_SNAPSHOT=path: if the file exists, restore the ColdFire machine from
//!   it and `path.dsp` (the SHARC engine's canonical state at the same point,
//!   so the DSP sees the same boot history) and skip the boot; otherwise boot
//!   with the DSP attached, save both files at ready (before the QA inputs)
//!   and continue. The SSI diagnostic (96 kHz) must match the saving run.
//!   DSP_ATTACH=ready with a snapshot is the old uncoupled mode (no .dsp).
//!   CF_DIGEST=1 prints the ColdFire state digest, the DSP export sha256 and
//!   the sha256 of the PCM produced since ready.
//!   DSP_THREAD=1: the DSP renders each period on its own thread while the
//!   ColdFire runs the next frame (`ThreadedPeer`); results are identical to
//!   the default synchronous run (compare CF_DIGEST output).
//!   AUDIO=1 (needs `--features play`): play the PCM through the default
//!   output device as it is produced (silence while the emulator is behind,
//!   which is nearly always: it runs far slower than real time).
//!   AUDIO_BUFFER=SECONDS: hold the audio back until that much has been
//!   produced, then play it in real time (implies AUDIO=1); at the end of the
//!   run whatever is left is played out. No device: a message, the run goes on.
//! Build: SHARC_GEN_DIR=<generated dir> cargo build --release --features sharc
//!   [,play] --example sharc_live
//! The DSP state is a mid-run continuation, so it does not match the
//! ColdFire's boot-time view of the link; see the findings notes.

mod common;

use elektron_native_boot::{
    Emulator,
    sharc_peer::{
        DEFAULT_PERIOD, DN2_IDLE_RANGE, NativeDsp, SharcPeer, Shared, SharedDsp, ThreadedHandle,
        ThreadedPeer, open_dn2_engine,
    },
};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, rc::Rc};
use std::{env, fs, io::Write, time::Instant};

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

const DSP_MAGIC: &[u8; 8] = b"DT2DSP01";

/// Host view of the DSP engine: in-thread (`SharedDsp`) or on the worker.
enum DspCtl {
    Local(SharedDsp),
    Worker(ThreadedHandle),
}

impl DspCtl {
    /// Finish every frame sent so far (worker only).
    fn sync(&self) {
        if let DspCtl::Worker(h) = self {
            h.sync();
        }
    }
    /// (canonical state, instruction counter) after every frame so far.
    fn export(&self) -> (Vec<u8>, u64) {
        match self {
            DspCtl::Local(d) => (d.export(), d.0.borrow().s.icount),
            DspCtl::Worker(h) => h.export(),
        }
    }
}

/// Optional live playback of the PCM the peer has produced so far.
struct Audio {
    #[cfg(feature = "play")]
    player: Option<elektron_native_boot::pcm_play::PcmPlayer>,
    sent: usize,
}

impl Audio {
    fn from_env() -> Self {
        let buffer = env::var("AUDIO_BUFFER")
            .ok()
            .map(|v| v.parse::<f64>().unwrap());
        let on = buffer.is_some() || env::var("AUDIO").as_deref() == Ok("1");
        #[cfg(feature = "play")]
        {
            use elektron_native_boot::pcm_play::{Mode, PcmPlayer};
            let player = on
                .then(|| {
                    let mode = buffer.map_or(Mode::Live, Mode::Buffer);
                    match PcmPlayer::spawn(mode) {
                        Ok(p) => {
                            println!("audio: {} at {} Hz, mode {mode:?}", p.device, p.device_rate);
                            Some(p)
                        }
                        Err(e) => {
                            eprintln!("audio disabled: {e}");
                            None
                        }
                    }
                })
                .flatten();
            Audio { player, sent: 0 }
        }
        #[cfg(not(feature = "play"))]
        {
            if on {
                eprintln!("AUDIO needs `--features play`; continuing silent");
            }
            Audio { sent: 0 }
        }
    }

    /// Hand the newly produced PCM to the player (never blocks).
    fn feed(&mut self, shared: &Rc<RefCell<Shared>>) {
        #[cfg(feature = "play")]
        if let Some(p) = &self.player {
            let sh = shared.borrow();
            if sh.pcm.len() > self.sent {
                p.push(&sh.pcm[self.sent..]);
                self.sent = sh.pcm.len();
            }
        }
        #[cfg(not(feature = "play"))]
        let _ = (shared, &mut self.sent);
    }

    fn finish(self) {
        #[cfg(feature = "play")]
        if let Some(p) = self.player {
            println!("audio: playing out the buffered PCM");
            p.finish();
        }
    }
}

fn wav_f32(pcm: &[f32]) -> Vec<u8> {
    let data = (pcm.len() * 4) as u32;
    let mut o = Vec::with_capacity(44 + pcm.len() * 4);
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&(36 + data).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    o.extend_from_slice(&2u16.to_le_bytes());
    o.extend_from_slice(&48_000u32.to_le_bytes());
    o.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
    o.extend_from_slice(&8u16.to_le_bytes());
    o.extend_from_slice(&32u16.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        o.extend_from_slice(&s.to_le_bytes());
    }
    o
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: sharc_live SYX DSP_STATE DSP_IMAGE OUT.wav [EXTRA_AFTER_READY]");
        std::process::exit(2);
    }
    let syx = fs::read(&args[1]).unwrap();
    let state = fs::read(&args[2]).unwrap();
    let image = fs::read(&args[3]).unwrap();
    let out = &args[4];
    let extra: u64 = args.get(5).map_or(250_000_000, |v| v.parse().unwrap());
    let period: u32 = env::var("DSP_PERIOD").map_or(DEFAULT_PERIOD, |v| v.parse().unwrap());
    let base: u64 = env::var("DSP_CLOCK_BASE").map_or(573_627_620, |v| v.parse().unwrap());
    // CF_SNAPSHOT=path: coupled snapshot (ColdFire + DSP engine state at
    // ready). Only DSP_ATTACH=ready gives the old uncoupled mode.
    let snap_path = common::snapshot_path();
    let attach_at_ready = env::var("DSP_ATTACH").as_deref() == Ok("ready");
    let dsp_path = snap_path.as_ref().map(|p| {
        let mut o = p.clone().into_os_string();
        o.push(".dsp");
        std::path::PathBuf::from(o)
    });
    // A coupled restore replaces the DSP state with the saved one.
    let restoring = !attach_at_ready
        && snap_path.as_ref().is_some_and(|p| p.exists())
        && dsp_path.as_ref().is_some_and(|p| p.exists());
    let mut dsp_instructions0 = 0u64;
    let mut base = base;
    let state = match (&dsp_path, restoring) {
        (Some(p), true) => {
            let f = fs::read(p).unwrap();
            assert_eq!(&f[..8], DSP_MAGIC, "bad .dsp file");
            // The instruction clock continues from the saved tick.
            base = u64::from_le_bytes(f[8..16].try_into().unwrap());
            dsp_instructions0 = u64::from_le_bytes(f[16..24].try_into().unwrap());
            f[24..].to_vec()
        }
        _ => state,
    };

    let threaded = env::var("DSP_THREAD").as_deref() == Ok("1");
    let (peer, dsp, shared): (Box<dyn periph::dspi::Peer>, DspCtl, _) = if threaded {
        let (image, state) = (image.clone(), state.clone());
        let (p, h, sh) = ThreadedPeer::spawn(
            move || open_dn2_engine(&image, &state, base).map(NativeDsp),
            period,
            DN2_IDLE_RANGE,
        )
        .expect("DSP worker");
        (p.boxed(), DspCtl::Worker(h), sh)
    } else {
        let d = SharedDsp::new(open_dn2_engine(&image, &state, base).expect("DSP engine"));
        let (p, sh) = SharcPeer::new(d.clone(), period);
        (p.boxed(), DspCtl::Local(d), sh)
    };
    shared.borrow_mut().attached = !attach_at_ready;
    shared.borrow_mut().dsp_instructions = dsp_instructions0;
    let mut emu = Emulator::new(&syx, None).unwrap();
    emu.enable_ssi_diagnostic(96_000).unwrap();

    let mut script = common::Script::from_env();
    let t0 = Instant::now();
    let mut saved = false;
    let mut pcm_base = 0usize;
    if let Some(p) = snap_path.as_deref()
        && common::restore(&mut emu, p, &mut script).is_some()
    {
        shared.borrow_mut().attached = true;
        assert!(
            attach_at_ready || restoring,
            "coupled restore needs {}",
            dsp_path.as_ref().unwrap().display()
        );
    }
    emu.set_dspi2_peer(peer);
    let mut audio = Audio::from_env();
    let mut next = 0;
    let end = loop {
        let snap = emu.step_chunk(250_000);
        let s = &snap.status;
        if let Some(p) = &snap_path {
            if common::save_if_ready(&mut emu, p, &script, s.ready, &mut saved) && !attach_at_ready
            {
                let (exported, icount) = dsp.export(); // also syncs the worker
                let mut f = DSP_MAGIC.to_vec();
                let tick = base.wrapping_add(icount);
                f.extend(tick.to_le_bytes());
                f.extend(shared.borrow().dsp_instructions.to_le_bytes());
                f.extend(exported);
                fs::write(dsp_path.as_ref().unwrap(), &f).unwrap();
                pcm_base = shared.borrow().pcm.len();
                println!("saved DSP state ({} KB)", f.len() >> 10);
            }
        }
        if script.poll(&mut emu, s.icount, s.ready) && attach_at_ready {
            shared.borrow_mut().attached = true;
        }
        audio.feed(&shared);
        if s.icount >= next {
            let sh = shared.borrow();
            println!(
                "cf_icount={} frames={} dsp_instr={} halted={:?} wall={:.0}s",
                s.icount,
                sh.frames.len(),
                sh.dsp_instructions,
                sh.halted,
                t0.elapsed().as_secs_f64()
            );
            next = s.icount + 50_000_000;
        }
        if script.ready_at.is_some_and(|at| s.icount - at >= extra)
            || s.error.is_some()
            || s.icount >= 3_000_000_000
        {
            println!("end cf_icount={} error={:?}", s.icount, s.error);
            break s.icount;
        }
    };
    dsp.sync();
    let wall = t0.elapsed().as_secs_f64();
    if env::var_os("CF_DIGEST").is_some() {
        let sh = shared.borrow();
        let pcm: Vec<u8> = sh.pcm[pcm_base..]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        println!("cf_state_digest={}", emu.state_digest().unwrap());
        println!("dsp_export_sha256={}", sha_hex(&dsp.export().0));
        println!(
            "pcm_since_ready_sha256={} samples={}",
            sha_hex(&pcm),
            pcm.len() / 4
        );
    }
    audio.feed(&shared);
    let sh = shared.borrow();
    fs::write(out, wav_f32(&sh.pcm)).unwrap();
    let raw: Vec<u8> = sh.raw.iter().flat_map(|w| w.to_le_bytes()).collect();
    fs::write(format!("{out}.q31"), raw).unwrap();
    let mut csv = std::io::BufWriter::new(fs::File::create(format!("{out}.csv")).unwrap());
    writeln!(
        csv,
        "frame,first_word,tx_hash,reply_hash,reply_nonzero,busy,idle,executed,block"
    )
    .unwrap();
    for (i, f) in sh.frames.iter().enumerate() {
        writeln!(
            csv,
            "{i},{},{:016x},{:016x},{},{},{},{},{}",
            f.first_word,
            f.tx_hash,
            f.reply_hash,
            f.reply_nonzero_bytes,
            f.busy,
            f.reached_idle as u8,
            f.executed,
            f.block as u8
        )
        .unwrap();
    }
    let mut busy: Vec<u32> = sh
        .frames
        .iter()
        .filter(|f| f.executed != 0)
        .map(|f| f.busy)
        .collect();
    busy.sort_unstable();
    let audio_s = sh.frames.len() as f64 * 32.0 / 48_000.0;
    println!("frames={} audio_seconds={audio_s:.2}", sh.frames.len());
    println!(
        "wall={wall:.1}s real_time_factor={:.1}x slower",
        wall / audio_s.max(1e-9)
    );
    println!(
        "cf_instructions={end} dsp_instructions={}",
        sh.dsp_instructions
    );
    if !busy.is_empty() {
        let total: u64 = busy.iter().map(|&b| b as u64).sum();
        println!(
            "dsp_busy min={} median={} max={} idle_share={:.3} frames_never_idle={}",
            busy[0],
            busy[busy.len() / 2],
            busy[busy.len() - 1],
            1.0 - total as f64 / (busy.len() as f64 * period as f64),
            sh.frames
                .iter()
                .filter(|f| f.executed != 0 && !f.reached_idle)
                .count()
        );
    }
    println!(
        "nonzero_replies={} missing_blocks={} halted={:?}",
        sh.nonzero_replies, sh.missing_blocks, sh.halted
    );
    println!("first_words={:04x?}", sh.first_words);
    drop(sh);
    audio.finish();
}
