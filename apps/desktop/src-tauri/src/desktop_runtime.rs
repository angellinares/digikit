//! The desktop core is the shared coupled runtime (`packages/emulator/coupled`, also used by
//! the headless `coupled-run`) behind the shared controller (`packages/emulator/control`).
//! Configured local DN2 ready assets attach coupled audio on normal startup. The
//! fixture gates stay here, as thin tests, so their names and asserted hashes
//! keep their place for the tooling that runs them.
#[allow(unused_imports)]
pub use digi_coupled::{AudioMode, AudioOptions};

/// The PGO training workload: the `coupled_ready_workload` gate without its pins, runnable from
/// the app binary (`--pgo-train`) so the profile is trained on the same crate
/// hashes the shipped binary has. Needs the private DN2 ready fixtures.
#[cfg(feature = "coupled-audio")]
pub fn pgo_training_workload() -> Result<(), String> {
    std::panic::catch_unwind(|| {
        digi_coupled::gates::coupled_ready_workload();
    })
    .map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "gate panicked".into())
    })
}

#[cfg(feature = "coupled-audio")]
mod coupled {
    #[cfg(test)]
    mod tests {
        use digi_coupled::gates;

        #[test]
        #[ignore = "private DN2 ready fixtures and locally generated DSP; bounded 100M-instruction gate"]
        fn coupled_ready_exactness() {
            gates::coupled_ready_exactness();
        }

        #[test]
        #[ignore = "private DN2 ready fixtures; captures the desktop Audition Trig 1 path without the shared QA script"]
        fn desktop_trig1_capture_without_qa_script() {
            gates::desktop_trig1_capture_without_qa_script();
        }

        #[test]
        #[ignore = "private DN2 fixtures; missing-device path must leave coupling intact"]
        fn missing_device_keeps_coupled_runtime_and_records_error() {
            gates::missing_device_keeps_coupled_runtime_and_records_error();
        }

        #[test]
        #[ignore = "private DN2 fixtures and an audible default output device"]
        fn default_output_receives_nonzero_emulated_pcm_and_tears_down() {
            gates::default_output_receives_nonzero_emulated_pcm_and_tears_down();
        }
    }
}
