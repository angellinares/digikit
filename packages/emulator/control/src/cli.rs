//! Command-line arguments shared by the desktop app and `digi-emud`.

use std::path::PathBuf;

use digi_coupled::{AudioMode, AudioOptions};

pub const USAGE: &str = "usage: digiemu [SYX] [--card-image FILE [--card-folder DIR [--no-card-sync]]] [--mirror-dir DIR] [--device-folders] [--control|--no-control] [--runtime|--no-runtime] [--paused] [--run-dir DIR] [--coupled|--auto-coupled --dsp-image FILE --dsp-state FILE --cf-snapshot FILE [--dsp-patch FILE] [--audio-buffer SECONDS] [--audio-mode rebuffer|stretch|gap] [--also-coupled DSP_IMAGE DSP_STATE CF_SNAPSHOT SYX]... [--scenario NAME_OR_FILE] [--report PATH] [--no-audio]] [--dsp zero --cf-snapshot FILE]";

#[derive(Clone, Debug, Default)]
pub struct StartupArgs {
    pub syx: Option<PathBuf>,
    pub card_image: Option<PathBuf>,
    /// The +Drive folder `--card-image` was built from (`--card-folder`): the firmware's changes are exported
    /// to it on quit unless `--no-card-sync`.
    pub card_folder: Option<PathBuf>,
    /// False with `--no-card-sync`.
    pub card_sync: bool,
    /// The folder the module store is mirrored to for the first load (`--mirror-dir`): only the first device's default.
    pub mirror_dir: Option<PathBuf>,
    /// `--device-folders`: one folder per device (`plus_drive/<device>/` or the user's choice): the module store is mirrored to it
    /// and the card of a load is built from it. `--no-card-sync` turns the mirror and the export on quit off.
    pub device_folders: bool,
    pub audio: Option<AudioOptions>,
    /// `--also-coupled`: the ready inputs of another device (a build with the second generated core).
    pub audio_alt: Vec<AudioOptions>,
    pub scenario: Option<String>,
    pub report: Option<PathBuf>,
    /// `--control` / `--no-control`; None leaves the host's default.
    pub control: Option<bool>,
    /// Start with the guest paused (control clients step it).
    pub paused: bool,
    /// Directory for sockets and the discovery file (default `out/native/run`).
    pub run_dir: Option<PathBuf>,
    /// `--runtime` / `--no-runtime`: load the startup .syx with the digikit runtime patched in (`firmware.load {runtime}`);
    /// None leaves the host's default.
    pub runtime: Option<bool>,
}

pub fn parse_values(
    values: impl IntoIterator<Item = std::ffi::OsString>,
    coupled_build: bool,
) -> Result<StartupArgs, String> {
    let mut values = values.into_iter();
    let (mut syx, mut card_image, mut image, mut state, mut snapshot) =
        (None, None, None, None, None);
    let mut audio_profile_syx = None;
    let mut coupled = false;
    let mut zero = false;
    let mut automatic = false;
    let mut buffer = None;
    let mut no_audio = false;
    let mut audio_mode = None;
    let mut dsp_patch = None;
    let mut scenario = None;
    let mut report = None;
    let mut control = None;
    let mut paused = false;
    let mut run_dir = None;
    let mut card_folder = None;
    let mut card_sync = true;
    let mut mirror_dir = None;
    let mut device_folders = false;
    let mut runtime = None;
    let mut also: Vec<[PathBuf; 4]> = Vec::new();
    while let Some(value) = values.next() {
        let mut path = |label: &str| -> Result<PathBuf, String> {
            values
                .next()
                .map(Into::into)
                .ok_or_else(|| format!("{label} requires a file"))
        };
        if value == "--card-image" {
            card_image = Some(path("--card-image")?);
        } else if value == "--card-folder" {
            card_folder = Some(path("--card-folder")?);
        } else if value == "--no-card-sync" {
            card_sync = false;
        } else if value == "--mirror-dir" {
            mirror_dir = Some(path("--mirror-dir")?);
        } else if value == "--device-folders" {
            device_folders = true;
        } else if value == "--control" {
            control = Some(true);
        } else if value == "--no-control" {
            control = Some(false);
        } else if value == "--runtime" {
            runtime = Some(true);
        } else if value == "--no-runtime" {
            runtime = Some(false);
        } else if value == "--paused" {
            paused = true;
        } else if value == "--run-dir" {
            run_dir = Some(path("--run-dir")?);
        } else if value == "--coupled" {
            coupled = true;
        } else if value == "--auto-coupled" {
            coupled = true;
            automatic = true;
        } else if value == "--audio-profile-syx" {
            audio_profile_syx = Some(path("--audio-profile-syx")?);
        } else if value == "--also-coupled" {
            also.push([
                path("--also-coupled DSP_IMAGE")?,
                path("--also-coupled DSP_STATE")?,
                path("--also-coupled CF_SNAPSHOT")?,
                path("--also-coupled SYX")?,
            ]);
        } else if value == "--dsp-image" {
            image = Some(path("--dsp-image")?);
        } else if value == "--dsp-state" {
            state = Some(path("--dsp-state")?);
        } else if value == "--cf-snapshot" {
            snapshot = Some(path("--cf-snapshot")?);
        } else if value == "--dsp" {
            let mode = values.next().ok_or("--dsp requires zero or sharc")?;
            zero = match mode.to_str() {
                Some("zero") => true,
                Some("sharc") => false,
                _ => return Err("--dsp is zero or sharc".into()),
            };
        } else if value == "--dsp-patch" {
            dsp_patch = Some(path("--dsp-patch")?);
        } else if value == "--no-audio" {
            no_audio = true;
        } else if value == "--audio-buffer" {
            let seconds = values.next().ok_or("--audio-buffer requires seconds")?;
            let seconds: f64 = seconds
                .to_str()
                .ok_or("invalid --audio-buffer")?
                .parse()
                .map_err(|_| "invalid --audio-buffer")?;
            if !seconds.is_finite() || !(0.0..=10.0).contains(&seconds) {
                return Err("--audio-buffer must be finite and between 0 and 10 seconds".into());
            }
            buffer = Some(seconds);
        } else if value == "--audio-mode" {
            let mode = values.next().ok_or("--audio-mode requires a mode")?;
            audio_mode = Some(AudioMode::parse(
                mode.to_str().ok_or("invalid --audio-mode")?,
            )?);
        } else if value == "--scenario" {
            let spec = values.next().ok_or("--scenario requires a name or file")?;
            scenario = Some(
                spec.into_string()
                    .map_err(|_| "invalid --scenario".to_string())?,
            );
        } else if value == "--report" {
            report = Some(path("--report")?);
        } else if value.to_string_lossy().starts_with("--") {
            return Err(format!("unknown option: {}", value.to_string_lossy()));
        } else if syx.is_none() {
            syx = Some(value.into());
        } else {
            return Err(USAGE.into());
        }
    }
    if card_folder.is_some() && card_image.is_none() {
        return Err("--card-folder needs --card-image".into());
    }
    if !coupled_build && (scenario.is_some() || report.is_some()) {
        return Err("--scenario needs a coupled-audio build".into());
    }
    if syx.is_none() && audio_profile_syx.is_some() && !automatic {
        return Err("--audio-profile-syx requires --auto-coupled".into());
    }
    let audio = if zero {
        if coupled || image.is_some() || state.is_some() || dsp_patch.is_some() {
            return Err(
                "--dsp zero takes only --cf-snapshot (no --coupled, --dsp-image, --dsp-state or --dsp-patch)"
                    .into(),
            );
        }
        Some(AudioOptions {
            snapshot: snapshot.take().ok_or("--dsp zero requires --cf-snapshot")?,
            zero: true,
            ..Default::default()
        })
    } else if coupled {
        if syx.is_none() && (!automatic || audio_profile_syx.is_none()) {
            return Err(
                "empty coupled startup requires --auto-coupled and explicit --audio-profile-syx"
                    .into(),
            );
        }
        Some(AudioOptions {
            image: image.ok_or("--coupled requires --dsp-image")?,
            state: state.ok_or("--coupled requires --dsp-state")?,
            snapshot: snapshot.ok_or("--coupled requires --cf-snapshot")?,
            buffer_seconds: buffer.unwrap_or(0.0),
            playback: !no_audio,
            automatic,
            expected_syx: if automatic {
                Some(audio_profile_syx.unwrap_or_else(|| {
                    std::env::var_os("DIGI_EMU_SYX")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("firmware/Digitone_II_OS1.12.syx"))
                }))
            } else {
                None
            },
            link_timing: false,
            audio_mode: audio_mode.unwrap_or_default(),
            dsp_patch: dsp_patch.take(),
            zero: false,
        })
    } else {
        if image.is_some()
            || state.is_some()
            || snapshot.is_some()
            || buffer.is_some()
            || audio_mode.is_some()
            || dsp_patch.is_some()
            || scenario.is_some()
            || report.is_some()
        {
            return Err("audio options require --coupled".into());
        }
        None
    };
    // The other devices' ready inputs: the primary's playback settings, the profile check on their own firmware.
    let audio_alt = match (&audio, also.is_empty()) {
        (_, true) => Vec::new(),
        (Some(primary), false) if !primary.zero => also
            .into_iter()
            .map(|[image, state, snapshot, syx]| AudioOptions {
                image,
                state,
                snapshot,
                automatic: true,
                expected_syx: Some(syx),
                dsp_patch: None,
                ..primary.clone()
            })
            .collect(),
        _ => return Err("--also-coupled needs --coupled or --auto-coupled".into()),
    };
    Ok(StartupArgs {
        syx,
        card_image,
        card_folder,
        card_sync,
        mirror_dir,
        device_folders,
        audio,
        audio_alt,
        scenario,
        report,
        control,
        paused,
        run_dir,
        runtime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<StartupArgs, String> {
        parse_values(args.iter().map(std::ffi::OsString::from), true)
    }

    #[test]
    fn control_flags_and_run_dir_parse() {
        let a = parse(&["x.syx", "--control", "--paused", "--run-dir", "d"]).unwrap();
        assert_eq!(a.control, Some(true));
        assert!(a.paused);
        assert_eq!(a.run_dir, Some("d".into()));
        assert_eq!(parse(&["--no-control"]).unwrap().control, Some(false));
        assert_eq!(parse(&[]).unwrap().control, None);
        assert_eq!(parse(&["--runtime"]).unwrap().runtime, Some(true));
        assert_eq!(parse(&["--no-runtime"]).unwrap().runtime, Some(false));
        assert_eq!(parse(&[]).unwrap().runtime, None);
    }

    #[test]
    fn card_folder_syncs_unless_switched_off() {
        let a = parse(&["x.syx", "--card-image", "c.img", "--card-folder", "pd"]).unwrap();
        assert_eq!(a.card_folder, Some("pd".into()));
        assert!(a.card_sync);
        let a = parse(&["--card-image", "c", "--card-folder", "pd", "--no-card-sync"]).unwrap();
        assert!(!a.card_sync);
        assert!(parse(&["--card-folder", "pd"]).is_err());
        assert_eq!(parse(&[]).unwrap().card_folder, None);
    }

    #[test]
    fn device_folders_is_a_switch() {
        assert!(!parse(&[]).unwrap().device_folders);
        let a = parse(&["x.syx", "--device-folders", "--no-card-sync"]).unwrap();
        assert!(a.device_folders && !a.card_sync);
        // the first load's folder is a default, not a switch
        assert_eq!(
            parse(&["--mirror-dir", "m"]).unwrap().mirror_dir,
            Some("m".into())
        );
    }

    #[test]
    fn dsp_zero_needs_only_the_cf_snapshot() {
        let a = parse(&["x.syx", "--dsp", "zero", "--cf-snapshot", "s.snap"]).unwrap();
        let audio = a.audio.unwrap();
        assert!(audio.zero);
        assert_eq!(audio.snapshot, PathBuf::from("s.snap"));
        assert!(parse(&["--dsp", "zero"]).is_err());
        assert!(parse(&["--dsp", "zero", "--cf-snapshot", "s", "--dsp-image", "i"]).is_err());
        assert!(parse(&["--dsp", "other"]).is_err());
        assert!(parse(&["--dsp", "sharc"]).unwrap().audio.is_none());
    }

    #[test]
    fn also_coupled_adds_another_devices_ready_inputs() {
        let a = parse(&[
            "x.syx",
            "--auto-coupled",
            "--dsp-image",
            "i",
            "--dsp-state",
            "s",
            "--cf-snapshot",
            "c",
            "--no-audio",
            "--also-coupled",
            "i2",
            "s2",
            "c2",
            "y.syx",
        ])
        .unwrap();
        assert_eq!(a.audio_alt.len(), 1);
        let alt = &a.audio_alt[0];
        assert_eq!(
            (&alt.image, &alt.state, &alt.snapshot, &alt.expected_syx),
            (
                &"i2".into(),
                &"s2".into(),
                &"c2".into(),
                &Some("y.syx".into())
            )
        );
        assert!(alt.automatic && !alt.playback);
        assert!(parse(&["--also-coupled", "i2", "s2", "c2", "y.syx"]).is_err());
        assert!(
            parse(&[
                "--coupled",
                "--dsp-image",
                "i",
                "--dsp-state",
                "s",
                "--cf-snapshot",
                "c",
                "--also-coupled",
                "i2"
            ])
            .is_err()
        );
        assert!(parse(&[]).unwrap().audio_alt.is_empty());
    }

    #[test]
    fn empty_automatic_coupled_startup_retains_explicit_profile_and_inputs() {
        let a = parse(&[
            "--auto-coupled",
            "--audio-profile-syx",
            "stock.syx",
            "--dsp-image",
            "i",
            "--dsp-state",
            "s",
            "--cf-snapshot",
            "c",
            "--no-audio",
        ])
        .unwrap();
        assert!(a.syx.is_none());
        let audio = a.audio.unwrap();
        assert!(audio.automatic && !audio.zero && !audio.playback);
        assert_eq!(audio.expected_syx, Some(PathBuf::from("stock.syx")));
        assert_eq!(
            (audio.image, audio.state, audio.snapshot),
            ("i".into(), "s".into(), "c".into())
        );
    }

    #[test]
    fn coupled_startup_refuses_unguarded_empty_or_ignored_profiles() {
        let inputs = ["--dsp-image", "i", "--dsp-state", "s", "--cf-snapshot", "c"];
        for mode in ["--auto-coupled", "--coupled"] {
            let args = [vec![mode], inputs.to_vec()].concat();
            assert!(
                parse(&args).is_err(),
                "empty {mode} must not use an implicit profile"
            );
        }
        let args = [
            vec!["--coupled", "--audio-profile-syx", "stock.syx"],
            inputs.to_vec(),
        ]
        .concat();
        assert!(
            parse(&args).is_err(),
            "empty plain coupled mode cannot use a profile"
        );
        assert!(parse(&["--audio-profile-syx", "stock.syx"]).is_err());
        assert!(parse(&["--auto-coupled", "--audio-profile-syx", "stock.syx"]).is_err());
    }

    #[test]
    fn automatic_startup_with_firmware_preserves_explicit_profile_identity() {
        let a = parse(&[
            "patched.syx",
            "--auto-coupled",
            "--audio-profile-syx",
            "stock.syx",
            "--dsp-image",
            "i",
            "--dsp-state",
            "s",
            "--cf-snapshot",
            "c",
        ])
        .unwrap();
        assert_eq!(a.syx, Some(PathBuf::from("patched.syx")));
        assert_eq!(
            a.audio.unwrap().expected_syx,
            Some(PathBuf::from("stock.syx"))
        );
        let plain = parse(&[
            "x.syx",
            "--coupled",
            "--audio-profile-syx",
            "ignored.syx",
            "--dsp-image",
            "i",
            "--dsp-state",
            "s",
            "--cf-snapshot",
            "c",
        ])
        .unwrap();
        assert_eq!(plain.syx, Some(PathBuf::from("x.syx")));
        assert_eq!(plain.audio.unwrap().expected_syx, None);
    }
}
