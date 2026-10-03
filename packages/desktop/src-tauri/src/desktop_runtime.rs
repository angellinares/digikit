//! Actor-owned desktop core. Configured local DN2 ready assets attach coupled audio on normal startup.
use elektron_native_boot::{Emulator, Snapshot};
use emmc_card::Card;
use serde_json::Value;
use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    time::Instant,
};

#[derive(Clone, Debug)]
pub struct AudioOptions {
    pub image: PathBuf,
    pub state: PathBuf,
    pub snapshot: PathBuf,
    pub buffer_seconds: f64,
    pub playback: bool,
    /// The launcher-provided ready session may fall back for files picked in
    /// the normal panel. Explicit --coupled remains strict.
    pub automatic: bool,
    pub expected_syx: Option<PathBuf>,
}

pub struct DesktopRuntime {
    emulator: Emulator,
    #[cfg(feature = "coupled-audio")]
    audio: Option<coupled::AudioSession>,
}
fn matches_automatic_profile(options: &AudioOptions, syx: &[u8]) -> bool {
    !options.automatic
        || options
            .expected_syx
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .is_some_and(|expected| expected == syx)
}
impl Deref for DesktopRuntime {
    type Target = Emulator;
    fn deref(&self) -> &Emulator {
        &self.emulator
    }
}
impl DerefMut for DesktopRuntime {
    fn deref_mut(&mut self) -> &mut Emulator {
        &mut self.emulator
    }
}
impl DesktopRuntime {
    pub fn new(
        syx: &[u8],
        card: Option<Card>,
        options: Option<&AudioOptions>,
    ) -> Result<Self, String> {
        let options = options.filter(|options| matches_automatic_profile(options, syx));
        let mut runtime = Self {
            emulator: Emulator::new(syx, card)?,
            #[cfg(feature = "coupled-audio")]
            audio: None,
        };
        runtime.attach(options)?;
        Ok(runtime)
    }
    fn attach(&mut self, options: Option<&AudioOptions>) -> Result<(), String> {
        let Some(options) = options else {
            return Ok(());
        };
        #[cfg(feature = "coupled-audio")]
        {
            self.audio = Some(coupled::AudioSession::new(&mut self.emulator, options)?);
            Ok(())
        }
        #[cfg(not(feature = "coupled-audio"))]
        {
            let _ = options;
            Err("--coupled requires the coupled-audio build feature".into())
        }
    }
    pub fn step_chunk(&mut self, budget: u32) -> Snapshot {
        #[cfg(feature = "coupled-audio")]
        let cf_step_start = self
            .audio
            .as_ref()
            .is_some_and(coupled::AudioSession::link_timing_enabled)
            .then(Instant::now);
        let snapshot = self.emulator.step_chunk(budget);
        #[cfg(feature = "coupled-audio")]
        if let Some(audio) = &mut self.audio {
            if let Some(start) = cf_step_start {
                audio.record_cf_step(start.elapsed().as_nanos());
            }
            audio.poll();
        }
        snapshot
    }
    pub fn snapshot(&mut self) -> Snapshot {
        #[cfg(feature = "coupled-audio")]
        if let Some(audio) = &mut self.audio {
            audio.sync();
        }
        self.emulator.snapshot()
    }
    pub fn diagnostics(&mut self) -> Value {
        #[cfg(feature = "coupled-audio")]
        if let Some(audio) = &mut self.audio {
            audio.sync();
        }
        let mut report =
            serde_json::to_value(self.emulator.diagnostics()).expect("diagnostic serialization");
        #[cfg(feature = "coupled-audio")]
        if let Some(audio) = &self.audio {
            let health = audio.report();
            report["sharc_execution_connected"] = true.into();
            report["pcm_output_connected"] = health["sink_connected"].clone();
            report["native_audio"] = health;
        }
        // Keep the same report shape for ordinary CF-only runs.
        #[cfg(not(feature = "coupled-audio"))]
        let _ = &mut report;
        report
    }
    /// Lightweight native-output observation for the periodic desktop UI poll.
    /// It deliberately does not synchronize the peer or serialize emulator diagnostics.
    pub fn audio_status(&mut self) -> Option<Value> {
        #[cfg(feature = "coupled-audio")]
        if let Some(audio) = &mut self.audio {
            audio.poll();
            return Some(audio.report());
        }
        None
    }
}

#[cfg(feature = "coupled-audio")]
mod coupled {
    use super::*;
    use elektron_native_boot::{
        pcm_play::{Mode, PcmPlayer, SOURCE_RATE},
        sharc_peer::{
            DEFAULT_PERIOD, DN2_IDLE_RANGE, NativeDsp, Shared, ThreadedHandle, ThreadedPeer,
            ThreadedTimingProfile, open_dn2_engine,
        },
    };
    use serde_json::json;
    use std::{
        cell::RefCell,
        fs::File,
        io::{self, Write},
        rc::Rc,
        time::Instant,
    };

    const PCM_CAPTURE_LIMIT: u64 = SOURCE_RATE as u64 * 2 * 10;

    struct PcmCapture {
        file: Option<File>,
        path: String,
        count: u64,
        error: Option<String>,
    }

    impl PcmCapture {
        fn from_env() -> Option<Self> {
            let path = std::env::var_os("DIGI_EMU_PCM_F32LE")?;
            let path = PathBuf::from(path);
            let name = path.display().to_string();
            match File::create(&path) {
                Ok(file) => Some(Self {
                    file: Some(file),
                    path: name,
                    count: 0,
                    error: None,
                }),
                Err(error) => Some(Self {
                    file: None,
                    path: name,
                    count: 0,
                    error: Some(format!("PCM capture open: {error}")),
                }),
            }
        }

        fn write(&mut self, pcm: &[f32]) {
            let Some(file) = &mut self.file else {
                return;
            };
            match write_pcm_f32le(file, &mut self.count, pcm) {
                Ok(()) => {}
                Err(error) => {
                    self.file = None;
                    self.error = Some(format!("PCM capture write: {error}"));
                }
            }
        }
    }

    fn write_pcm_f32le(writer: &mut impl Write, count: &mut u64, pcm: &[f32]) -> io::Result<()> {
        let samples = pcm
            .len()
            .min(PCM_CAPTURE_LIMIT.saturating_sub(*count) as usize);
        if samples == 0 {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(samples * size_of::<f32>());
        for sample in &pcm[..samples] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        writer.write_all(&bytes)?;
        *count += samples as u64;
        Ok(())
    }

    fn continuation(data: &[u8]) -> Result<(u64, u64, &[u8]), String> {
        if data.len() <= 24 || &data[..8] != b"DT2DSP01" {
            return Err("coupled restore requires a complete DT2DSP01 DSP sidecar".into());
        }
        Ok((
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            &data[24..],
        ))
    }
    pub(super) struct AudioSession {
        handle: ThreadedHandle,
        shared: Rc<RefCell<Shared>>,
        player: Option<PcmPlayer>,
        device_error: Option<String>,
        playback_requested: bool,
        frames: u64,
        samples: u64,
        initial_instructions: u64,
        started: Instant,
        link_timing: bool,
        cf_step_calls: u64,
        cf_step_wall_ns: u64,
        pcm_handoff_ns: u64,
        audio_poll_wall_ns: u64,
        audio_sync_wall_ns: u64,
        pcm_capture: Option<PcmCapture>,
        #[cfg(test)]
        captured_pcm: Vec<f32>,
    }
    impl AudioSession {
        fn new_with_player(
            emulator: &mut Emulator,
            options: &AudioOptions,
            player: impl FnOnce(Mode) -> Result<PcmPlayer, String>,
        ) -> Result<Self, String> {
            let profile = emulator.diagnostics();
            if (profile.device.as_str(), profile.version.as_str()) != ("dn2", "1.11") {
                return Err("desktop coupled audio supports only DN2 OS 1.11".into());
            }
            if !options.buffer_seconds.is_finite()
                || !(0.0..=10.0).contains(&options.buffer_seconds)
            {
                return Err("--audio-buffer must be finite and between 0 and 10 seconds".into());
            }
            let image = std::fs::read(&options.image).map_err(|e| format!("DSP image: {e}"))?;
            let sidecar = std::fs::read(&options.state).map_err(|e| format!("DSP state: {e}"))?;
            let (clock, initial_instructions, state) = continuation(&sidecar)?;
            let state = state.to_vec();
            let snapshot =
                std::fs::read(&options.snapshot).map_err(|e| format!("CF snapshot: {e}"))?;
            let link_timing = std::env::var("DN2_PROFILE_LINK").as_deref() == Ok("1");
            emulator.enable_ssi_diagnostic(96_000)?;
            emulator.load_state(&snapshot)?;
            let (peer, handle, shared) = ThreadedPeer::spawn_with_timing(
                move || open_dn2_engine(&image, &state, clock).map(NativeDsp),
                DEFAULT_PERIOD,
                DN2_IDLE_RANGE,
                link_timing,
            )?;
            shared.borrow_mut().dsp_instructions = initial_instructions;
            emulator.set_dspi2_peer(peer.boxed());
            let mode = if options.buffer_seconds == 0.0 {
                Mode::Live
            } else {
                Mode::Buffer(options.buffer_seconds)
            };
            let (player, device_error) = if options.playback {
                match player(mode) {
                    Ok(p) => (Some(p), None),
                    Err(e) => (None, Some(e)),
                }
            } else {
                (None, None)
            };
            let pcm_capture = PcmCapture::from_env();
            Ok(Self {
                handle,
                shared,
                player,
                device_error,
                playback_requested: options.playback,
                frames: 0,
                samples: 0,
                initial_instructions,
                started: Instant::now(),
                link_timing,
                cf_step_calls: 0,
                cf_step_wall_ns: 0,
                pcm_handoff_ns: 0,
                audio_poll_wall_ns: 0,
                audio_sync_wall_ns: 0,
                pcm_capture,
                #[cfg(test)]
                captured_pcm: Vec::new(),
            })
        }
        pub(super) fn new(emulator: &mut Emulator, options: &AudioOptions) -> Result<Self, String> {
            Self::new_with_player(emulator, options, PcmPlayer::spawn)
        }
        pub(super) fn link_timing_enabled(&self) -> bool {
            self.link_timing
        }
        pub(super) fn record_cf_step(&mut self, elapsed_ns: u128) {
            if self.link_timing {
                self.cf_step_calls = self.cf_step_calls.saturating_add(1);
                self.cf_step_wall_ns = self
                    .cf_step_wall_ns
                    .saturating_add(elapsed_ns.min(u64::MAX as u128) as u64);
            }
        }
        fn drain(&mut self) {
            let handoff_start = self.link_timing.then(Instant::now);
            let mut shared = self.shared.borrow_mut();
            self.frames += shared.frames.len() as u64;
            self.samples += shared.pcm.len() as u64;
            if let Some(capture) = &mut self.pcm_capture {
                capture.write(&shared.pcm);
            }
            if let Some(player) = &self.player {
                player.push(&shared.pcm);
            }
            #[cfg(test)]
            self.captured_pcm.extend_from_slice(&shared.pcm);
            // Host observation histories are not DSP state. Keep production storage bounded
            // by one host step; retain the peer's cumulative counters and halt information.
            shared.pcm.clear();
            shared.raw.clear();
            shared.frames.clear();
            if let Some(start) = handoff_start {
                self.pcm_handoff_ns = self
                    .pcm_handoff_ns
                    .saturating_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
            }
        }
        pub(super) fn poll(&mut self) {
            let wall_start = self.link_timing.then(Instant::now);
            self.handle.poll();
            self.drain();
            if let Some(start) = wall_start {
                self.audio_poll_wall_ns = self
                    .audio_poll_wall_ns
                    .saturating_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
            }
        }
        pub(super) fn sync(&mut self) {
            let wall_start = self.link_timing.then(Instant::now);
            self.handle.sync();
            self.drain();
            if let Some(start) = wall_start {
                self.audio_sync_wall_ns = self
                    .audio_sync_wall_ns
                    .saturating_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
            }
        }
        pub(super) fn report(&self) -> Value {
            let shared = self.shared.borrow();
            let sink = self.player.as_ref().map(PcmPlayer::stats);
            let output_device = self.player.as_ref().map(|player| player.device.clone());
            let connected = sink.as_ref().is_some_and(|s| s.error.is_none());
            let wall = self.started.elapsed().as_secs_f64();
            let audio = self.samples as f64 / 2.0 / SOURCE_RATE as f64;
            let mut report = json!({
                "schema_version": 1, "peer": "threaded", "sample_rate": SOURCE_RATE,
                "period_instructions": DEFAULT_PERIOD, "ssi_period_instructions": 96_000,
                "frames": self.frames, "source_pcm_frames": self.samples / 2,
                "source_audio_seconds": audio, "session_wall_seconds": wall,
                "audio_seconds_per_session_wall_second": if wall > 0.0 { audio / wall } else { 0.0 },
                "dsp_instructions_since_restore": shared.dsp_instructions.saturating_sub(self.initial_instructions),
                "missing_blocks": shared.missing_blocks, "nonzero_replies": shared.nonzero_replies,
                "halted": shared.halted, "flow_started": self.samples > 0,
                "playback_requested": self.playback_requested, "sink_connected": connected,
                "device_error": self.device_error, "output_device": output_device, "sink": sink,
            });
            if let Some(capture) = &self.pcm_capture {
                report["pcm_capture_path"] = capture.path.clone().into();
                report["pcm_capture_count"] = capture.count.into();
                report["pcm_capture_error"] = capture.error.clone().into();
            }
            if self.link_timing {
                let timing = self.handle.timing_profile().expect("enabled link timing");
                report["link_timing"] = link_timing_json(
                    timing,
                    self.cf_step_calls,
                    self.cf_step_wall_ns,
                    self.pcm_handoff_ns,
                    self.audio_poll_wall_ns,
                    self.audio_sync_wall_ns,
                );
            }
            report
        }
    }

    fn link_timing_json(
        timing: ThreadedTimingProfile,
        cf_step_calls: u64,
        cf_step_wall_ns: u64,
        pcm_handoff_ns: u64,
        audio_poll_wall_ns: u64,
        audio_sync_wall_ns: u64,
    ) -> Value {
        json!({
            "units": "nanoseconds",
            "durations_overlap": true,
            "note": "Pipelined link and nested/enclosing wall durations are not additive.",
            "frames_sent": timing.frames_sent,
            "frames_completed": timing.frames_completed,
            "host_enqueue_ns": timing.host_enqueue_ns,
            "host_reply_wait_ns": timing.host_reply_wait_ns,
            "host_done_wait_ns": timing.host_done_wait_ns,
            "host_collect_ns": timing.host_collect_ns,
            "worker_queue_ns": timing.worker_queue_ns,
            "worker_spi_ns": timing.worker_spi_ns,
            "worker_dsp_ns": timing.worker_dsp_ns,
            "worker_sport_ns": timing.worker_sport_ns,
            "cf_step_calls": cf_step_calls,
            "cf_step_wall_ns": cf_step_wall_ns,
            "pcm_handoff_ns": pcm_handoff_ns,
            "audio_poll_wall_ns": audio_poll_wall_ns,
            "audio_sync_wall_ns": audio_sync_wall_ns,
        })
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn continuation_requires_header_and_payload_and_keeps_both_counters() {
            for data in [vec![], vec![0; 24], b"DT2DSP01".to_vec()] {
                assert!(continuation(&data).is_err());
            }
            let mut data = b"DT2DSP01".to_vec();
            data.extend(123u64.to_le_bytes());
            data.extend(456u64.to_le_bytes());
            assert!(continuation(&data).is_err());
            data.extend([7, 8]);
            assert_eq!(continuation(&data).unwrap(), (123, 456, &[7, 8][..]));
        }
        #[test]
        fn pcm_capture_writer_uses_little_endian_and_respects_the_limit() {
            let mut bytes = Vec::new();
            let mut count = 0;
            write_pcm_f32le(&mut bytes, &mut count, &[-1.0, 0.0, 0.5]).unwrap();
            assert_eq!(count, 3);
            assert_eq!(
                bytes,
                [
                    (-1.0f32).to_le_bytes(),
                    0.0f32.to_le_bytes(),
                    0.5f32.to_le_bytes()
                ]
                .concat()
            );

            let mut bounded = Vec::new();
            let mut count = PCM_CAPTURE_LIMIT - 1;
            write_pcm_f32le(&mut bounded, &mut count, &[0.25, 0.75]).unwrap();
            assert_eq!(count, PCM_CAPTURE_LIMIT);
            assert_eq!(bounded, 0.25f32.to_le_bytes());
        }
        #[test]
        fn pcm_capture_writer_returns_the_write_error_without_advancing_count() {
            struct FailingWriter;
            impl std::io::Write for FailingWriter {
                fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::other("expected write failure"))
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut count = 0;
            assert!(write_pcm_f32le(&mut FailingWriter, &mut count, &[0.5]).is_err());
            assert_eq!(count, 0);
        }
        fn options() -> AudioOptions {
            let dir = PathBuf::from(
                std::env::var_os("DIGI_COUPLED_FIXTURES").expect("fixture directory"),
            );
            AudioOptions {
                image: dir.join("digi-audio-dn2-image.bin"),
                state: dir.join("digi-audio-m5.snap.dsp"),
                snapshot: dir.join("digi-audio-m5.snap"),
                buffer_seconds: 0.0,
                playback: false,
                automatic: false,
                expected_syx: None,
            }
        }
        fn syx() -> Vec<u8> {
            std::fs::read(std::env::var_os("DIGI_COUPLED_SYX").expect("DN2 SYX")).unwrap()
        }
        fn timing_delta(
            after: ThreadedTimingProfile,
            before: ThreadedTimingProfile,
        ) -> ThreadedTimingProfile {
            ThreadedTimingProfile {
                frames_sent: after.frames_sent.saturating_sub(before.frames_sent),
                frames_completed: after
                    .frames_completed
                    .saturating_sub(before.frames_completed),
                host_enqueue_ns: after.host_enqueue_ns.saturating_sub(before.host_enqueue_ns),
                host_reply_wait_ns: after
                    .host_reply_wait_ns
                    .saturating_sub(before.host_reply_wait_ns),
                host_done_wait_ns: after
                    .host_done_wait_ns
                    .saturating_sub(before.host_done_wait_ns),
                host_collect_ns: after.host_collect_ns.saturating_sub(before.host_collect_ns),
                worker_queue_ns: after.worker_queue_ns.saturating_sub(before.worker_queue_ns),
                worker_spi_ns: after.worker_spi_ns.saturating_sub(before.worker_spi_ns),
                worker_dsp_ns: after.worker_dsp_ns.saturating_sub(before.worker_dsp_ns),
                worker_sport_ns: after.worker_sport_ns.saturating_sub(before.worker_sport_ns),
            }
        }
        #[test]
        #[ignore = "private DN2 ready fixtures and locally generated DSP; bounded 100M-instruction gate"]
        fn coupled_ready_exactness() {
            use sha2::{Digest, Sha256};
            let mut runtime = DesktopRuntime::new(&syx(), None, Some(&options())).unwrap();
            let initial_snapshot = runtime.snapshot();
            assert!(
                initial_snapshot
                    .frame
                    .as_ref()
                    .is_some_and(|frame| frame.iter().any(|byte| *byte != 0)),
                "ready fixture should restore a visible LCD frame"
            );
            let initial = initial_snapshot.status;
            assert!(initial.ready);
            let mut script = crate::desktop_runtime::common::Script::from_env();
            script.poll(&mut runtime.emulator, initial.icount, true);
            let timing_before = runtime.audio.as_ref().unwrap().handle.timing_profile();
            let desktop_timing_before = runtime.audio.as_ref().map(|audio| {
                (
                    audio.cf_step_calls,
                    audio.cf_step_wall_ns,
                    audio.pcm_handoff_ns,
                    audio.audio_poll_wall_ns,
                    audio.audio_sync_wall_ns,
                )
            });
            let workload_start = Instant::now();
            let mut ended = false;
            for _ in 0..1000 {
                let status = runtime.step_chunk(250_000).status;
                assert!(status.error.is_none(), "{:?}", status.error);
                script.poll(&mut runtime.emulator, status.icount, status.ready);
                if status.icount - initial.icount >= 100_000_000 {
                    ended = true;
                    break;
                }
            }
            assert!(ended, "bounded workload did not finish");
            runtime.snapshot();
            println!(
                "coupled_workload_elapsed_seconds={:.6}",
                workload_start.elapsed().as_secs_f64()
            );
            if let Some(before) = timing_before {
                let after = runtime
                    .audio
                    .as_ref()
                    .unwrap()
                    .handle
                    .timing_profile()
                    .expect("enabled timing profile");
                let window = timing_delta(after, before);
                assert!(window.frames_completed > 0);
                assert!(window.frames_completed <= window.frames_sent);
                println!("native_audio_link_timing_window={window:?}");
                let after = runtime.audio.as_ref().unwrap();
                let (calls, cf_step, pcm_handoff, poll, sync) = desktop_timing_before.unwrap();
                println!(
                    "native_audio_desktop_timing_window=cf_step_calls={} cf_step_wall_ns={} pcm_handoff_ns={} audio_poll_wall_ns={} audio_sync_wall_ns={}",
                    after.cf_step_calls.saturating_sub(calls),
                    after.cf_step_wall_ns.saturating_sub(cf_step),
                    after.pcm_handoff_ns.saturating_sub(pcm_handoff),
                    after.audio_poll_wall_ns.saturating_sub(poll),
                    after.audio_sync_wall_ns.saturating_sub(sync),
                );
            }
            assert_eq!(
                runtime.emulator.state_digest().unwrap(),
                "fbace0f0cf9fbbb6f5551e5834e8d74b2dfd6c84e81503693924b0aff1a2cb2c"
            );
            let audio = runtime.audio.as_ref().unwrap();
            let pcm: Vec<u8> = audio
                .captured_pcm
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            if let Some(path) = std::env::var_os("DIGI_COUPLED_PCM_F32LE") {
                std::fs::write(path, &pcm).unwrap();
            }
            assert_eq!(audio.captured_pcm.len(), 72_896);
            assert_eq!(
                format!("{:x}", Sha256::digest(&pcm)),
                "38d2a3224d10e0b59cafa602ba1b8f8be31562efa07491b963ff4e127f3ff1f8"
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(audio.handle.export().0)),
                "05ac2ac2e0e270aefffa207dda09017360eee499bb1851d30271d3c78f304e19"
            );
            assert_eq!(audio.report()["missing_blocks"], 0);
            assert!(audio.shared.borrow().pcm.is_empty());
            assert!(audio.shared.borrow().raw.is_empty());
            assert!(audio.shared.borrow().frames.is_empty());
            let report = runtime.diagnostics();
            assert_eq!(report["sharc_execution_connected"], true);
            assert_eq!(report["pcm_output_connected"], false);
            println!("native_audio_health={}", report["native_audio"]);
        }

        #[test]
        #[ignore = "private DN2 ready fixtures; captures the desktop Audition Trig 1 path without the shared QA script"]
        fn desktop_trig1_capture_without_qa_script() {
            use sha2::{Digest, Sha256};

            let mut runtime = DesktopRuntime::new(&syx(), None, Some(&options())).unwrap();
            let initial = runtime.snapshot().status;
            assert!(initial.ready);

            let press_after = std::env::var("DIGI_DESKTOP_TRIG1_PRESS_AFTER")
                .ok()
                .map(|value| value.parse::<u64>().expect("valid press delay"))
                .unwrap_or(0);
            let hold = 40_000_000u64;
            let press_at = initial.icount + press_after;
            let mut status = initial;
            for _ in 0..1000 {
                if status.icount >= press_at {
                    break;
                }
                status = runtime
                    .step_chunk((press_at - status.icount).min(250_000) as u32)
                    .status;
                assert!(status.error.is_none(), "{:?}", status.error);
            }
            assert!(
                status.icount >= press_at,
                "did not reach Trig 1 press boundary"
            );
            runtime.button(25, true).unwrap();
            let held_from = runtime.snapshot().status.icount;
            for _ in 0..1000 {
                if status.icount.saturating_sub(held_from) >= hold {
                    break;
                }
                status = runtime
                    .step_chunk(
                        (hold - status.icount.saturating_sub(held_from)).min(250_000) as u32,
                    )
                    .status;
                assert!(status.error.is_none(), "{:?}", status.error);
            }
            assert!(
                status.icount.saturating_sub(held_from) >= hold,
                "did not hold Trig 1 for {hold} instructions"
            );
            runtime.button(25, false).unwrap();
            runtime.snapshot();

            let audio = runtime.audio.as_ref().unwrap();
            assert!(!audio.captured_pcm.is_empty(), "Trig 1 produced no PCM");
            assert!(audio.captured_pcm.iter().all(|sample| sample.is_finite()));
            let pcm: Vec<u8> = audio
                .captured_pcm
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            let digest = format!("{:x}", Sha256::digest(&pcm));
            println!(
                "desktop_trig1_pcm press_after={press_after} held={} samples={} sha256={digest}",
                status.icount.saturating_sub(held_from),
                audio.captured_pcm.len(),
            );
            if let Some(path) = std::env::var_os("DIGI_DESKTOP_TRIG1_PCM_F32LE") {
                std::fs::write(path, pcm).unwrap();
            }
        }

        #[test]
        #[ignore = "private DN2 fixtures; missing-device path must leave coupling intact"]
        fn missing_device_keeps_coupled_runtime_and_records_error() {
            let mut emulator = Emulator::new(&syx(), None).unwrap();
            let mut opts = options();
            opts.playback = true;
            let mut audio = AudioSession::new_with_player(&mut emulator, &opts, |_| {
                Err("fake missing device".into())
            })
            .unwrap();
            emulator.step_chunk(250_000);
            audio.sync();
            assert!(audio.frames > 0);
            assert_eq!(audio.report()["device_error"], "fake missing device");
            assert_eq!(audio.report()["sink_connected"], false);
            assert!(audio.shared.borrow().halted.is_none());
        }

        #[test]
        #[ignore = "private DN2 fixtures and an audible default output device"]
        fn default_output_receives_nonzero_emulated_pcm_and_tears_down() {
            let mut options = options();
            options.playback = true;
            {
                let mut runtime = DesktopRuntime::new(&syx(), None, Some(&options)).unwrap();
                let initial = runtime.snapshot().status;
                assert!(initial.ready);
                let mut script = crate::desktop_runtime::common::Script::from_env();
                script.poll(&mut runtime.emulator, initial.icount, true);
                for _ in 0..400 {
                    let status = runtime.step_chunk(250_000).status;
                    assert!(status.error.is_none(), "{:?}", status.error);
                    script.poll(&mut runtime.emulator, status.icount, status.ready);
                    if runtime
                        .diagnostics()
                        .get("native_audio")
                        .and_then(|audio| audio.get("source_pcm_frames"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                        > 0
                    {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                let report = runtime.diagnostics();
                let audio = &report["native_audio"];
                if audio["sink_connected"] != true {
                    assert!(
                        audio["device_error"].as_str().is_some(),
                        "missing sink without a device error: {audio}"
                    );
                    eprintln!("default output unavailable: {}", audio["device_error"]);
                    return;
                }
                assert_eq!(audio["sink_connected"], true, "{audio}");
                assert_eq!(audio["flow_started"], true, "{audio}");
                assert!(
                    audio["source_pcm_frames"].as_u64().unwrap_or(0) > 0,
                    "{audio}"
                );
                assert!(
                    audio["sink"]["source_received_frames"]
                        .as_u64()
                        .unwrap_or(0)
                        > 0,
                    "{audio}"
                );
            }
        }
    }
}
#[cfg(test)]
mod profile_tests {
    use super::*;

    fn automatic(profile: PathBuf) -> AudioOptions {
        AudioOptions {
            image: PathBuf::new(),
            state: PathBuf::new(),
            snapshot: PathBuf::new(),
            buffer_seconds: 0.0,
            playback: false,
            automatic: true,
            expected_syx: Some(profile),
        }
    }

    #[test]
    fn automatic_profile_accepts_identical_bytes_and_rejects_another_firmware() {
        let path = std::env::temp_dir().join(format!(
            "digi-auto-profile-{}-{}.syx",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, [1, 2, 3, 4]).unwrap();
        let options = automatic(path.clone());
        assert!(matches_automatic_profile(&options, &[1, 2, 3, 4]));
        assert!(!matches_automatic_profile(&options, &[1, 2, 3, 5]));
        std::fs::remove_file(path).unwrap();
    }
}
#[cfg(all(test, feature = "coupled-audio"))]
#[path = "../../../../native/boot/examples/common/mod.rs"]
mod common;
