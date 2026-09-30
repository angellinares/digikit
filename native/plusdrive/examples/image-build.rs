//! Host-only parity probe for the portable sample-only image builder.

use std::{
    env, fs,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
};

use device_profile::Registry;
use plusdrive_format::{
    NamedSample, ProjectSeed, build_sample_image, build_sample_image_with_project, wav_to_native,
};
use sha2::{Digest, Sha256};

fn run() -> Result<(), String> {
    let mut args = env::args_os();
    let program = args.next().unwrap_or_default();
    let source = args.next().ok_or_else(|| {
        format!(
            "usage: {} WAV_DIRECTORY NEW_IMAGE [--syx VERIFIED_DT2_116.syx]",
            PathBuf::from(program).display()
        )
    })?;
    let output = args.next().ok_or_else(|| {
        "usage: image-build WAV_DIRECTORY NEW_IMAGE [--syx VERIFIED_DT2_116.syx]".to_owned()
    })?;
    let syx = match (args.next(), args.next()) {
        (None, None) => None,
        (Some(flag), Some(path)) if flag == "--syx" => Some(PathBuf::from(path)),
        _ => {
            return Err(
                "usage: image-build WAV_DIRECTORY NEW_IMAGE [--syx VERIFIED_DT2_116.syx]"
                    .to_owned(),
            );
        }
    };
    let source = PathBuf::from(source);
    let output = PathBuf::from(output);
    let mut samples = Vec::new();
    for entry in fs::read_dir(&source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
        {
            continue;
        }
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| "sample file name is not UTF-8".to_owned())?;
        let wav = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let sample = wav_to_native(&wav, Default::default())
            .map_err(|error| format!("{}: {error:?}", path.display()))?;
        samples.push(NamedSample { file_name, sample });
    }
    let built = if let Some(syx) = syx {
        let bytes = fs::read(&syx).map_err(|error| format!("{}: {error}", syx.display()))?;
        let syx_hash = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let firmware = dt2_firmware_loader::parse(&bytes)
            .map_err(|error| format!("{}: {error}", syx.display()))?;
        let main = firmware
            .sections
            .iter()
            .find(|section| section.id == 3)
            .ok_or("firmware has no MAIN section")?;
        let registry = Registry::embedded().map_err(|error| error.to_string())?;
        let (device, profile, _) = registry
            .boot_for_main(&syx_hash, &main.bytes)
            .map_err(|error| error.to_string())?;
        if device.short != "dt2" {
            return Err("project seeding is DT2-only".to_owned());
        }
        let contract = profile
            .plusdrive_project_contract
            .ok_or("firmware has no +Drive project contract")?;
        build_sample_image_with_project(
            samples,
            ProjectSeed {
                main: &main.bytes,
                contract,
            },
        )
        .map_err(|error| format!("build: {error:?}"))?
    } else {
        build_sample_image(samples).map_err(|error| format!("build: {error:?}"))?
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| format!("{}: {error}", output.display()))?;
    file.set_len(built.image.logical_len())
        .map_err(|error| error.to_string())?;
    for (&offset, bytes) in built.image.ranges() {
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(bytes))
            .map_err(|error| error.to_string())?;
    }
    println!(
        "files={} logical_bytes={} written_ranges={} project={:?}",
        built.files.len(),
        built.image.logical_len(),
        built.image.ranges().len(),
        built.project
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("image-build: {error}");
        std::process::exit(2);
    }
}
