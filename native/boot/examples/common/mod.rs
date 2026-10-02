//! Panel script shared by the headless examples: the handover QA sequence
//! (NO button at ready and +20M, encoder A at +40M) plus optional
//! NOTE_EVENTS=trig|play key events (see `dspi2_capture.rs`).

use elektron_native_boot::Emulator;

pub struct Script {
    events: Vec<(u64, u8, bool)>,
    next_event: usize,
    pub ready_at: Option<u64>,
    stage: u32,
}

impl Script {
    pub fn from_env() -> Self {
        let mut events = Vec::new();
        match std::env::var("NOTE_EVENTS").as_deref() {
            Ok("trig") => {
                for t in [60_000_000, 110_000_000, 160_000_000] {
                    events.push((t, 25, true));
                    events.push((t + 40_000_000, 25, false));
                }
            }
            Ok("play") => {
                events.push((60_000_000, 20, true));
                events.push((61_000_000, 20, false));
                events.push((200_000_000, 21, true));
                events.push((201_000_000, 21, false));
            }
            _ => {}
        }
        Self {
            events,
            next_event: 0,
            ready_at: None,
            stage: 0,
        }
    }

    /// Call after each chunk. Returns true on the chunk that first sees ready.
    pub fn poll(&mut self, emu: &mut Emulator, icount: u64, ready: bool) -> bool {
        let mut became_ready = false;
        if ready && self.ready_at.is_none() {
            self.ready_at = Some(icount);
            println!("ready_at={icount}");
            emu.button(12, true).unwrap();
            self.stage = 1;
            became_ready = true;
        }
        let Some(at) = self.ready_at else {
            return became_ready;
        };
        let elapsed = icount - at;
        let steps: [(u32, u64, Option<bool>); 4] = [
            (1, 1_000_000, Some(false)),
            (2, 20_000_000, Some(true)),
            (3, 21_000_000, Some(false)),
            (4, 40_000_000, None),
        ];
        for (stage, when, action) in steps {
            if self.stage == stage && elapsed >= when {
                match action {
                    Some(down) => emu.button(12, down).unwrap(),
                    None => emu.turn(1, 1).unwrap(),
                }
                self.stage = stage + 1;
            }
        }
        while self.next_event < self.events.len() && elapsed >= self.events[self.next_event].0 {
            let (t, code, down) = self.events[self.next_event];
            emu.button(code, down).unwrap();
            println!("event t=+{t} code={code} down={down} icount={icount}");
            self.next_event += 1;
        }
        became_ready
    }
}

/// `CF_SNAPSHOT=path`: restore the ColdFire machine from `path` if it exists
/// (skipping the ~850M-instruction boot), otherwise boot normally and save
/// the file at the first `ready` chunk boundary, before the QA inputs.
/// The DSPI2 peer/recorder therefore sees traffic from ready onward in both
/// modes. Enable any SSI diagnostic before calling `restore`.
pub fn snapshot_path() -> Option<std::path::PathBuf> {
    std::env::var_os("CF_SNAPSHOT").map(Into::into)
}

/// Restore `path` into the fresh `emu` if it exists. On success returns the
/// ready-point instruction count, with `script` primed as if ready had just
/// been seen; returns None when the file is absent.
pub fn restore(emu: &mut Emulator, path: &std::path::Path, script: &mut Script) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let t = std::time::Instant::now();
    emu.load_state(&bytes)
        .unwrap_or_else(|e| panic!("restore {}: {e}", path.display()));
    let s = emu.snapshot().status;
    assert!(s.ready, "snapshot was not taken at ready");
    println!(
        "restored {} ({} MB) at icount={} in {:.1}s",
        path.display(),
        bytes.len() >> 20,
        s.icount,
        t.elapsed().as_secs_f64()
    );
    script.poll(emu, s.icount, true);
    Some(s.icount)
}

/// Save at the ready boundary (call after `step_chunk`, before `poll`).
/// Returns true on the call that wrote the file.
pub fn save_if_ready(
    emu: &mut Emulator,
    path: &std::path::Path,
    script: &Script,
    ready: bool,
    saved: &mut bool,
) -> bool {
    if *saved || !ready || script.ready_at.is_some() {
        return false;
    }
    let bytes = emu.save_state().expect("save snapshot");
    std::fs::write(path, &bytes).expect("write snapshot");
    println!("saved {} ({} MB)", path.display(), bytes.len() >> 20);
    *saved = true;
    true
}
