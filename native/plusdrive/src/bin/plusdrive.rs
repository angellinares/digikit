use device_profile::Registry;
use emmc_card::RandomAccessRead;
use plusdrive_format::{
    NamedSample, ProjectSeed, WavLimits, build_sample_image_with_options, list_image, wav_to_native,
};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Mutex,
};

struct FileImage {
    file: Mutex<fs::File>,
    len: u64,
}
impl RandomAccessRead for FileImage {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> usize {
        if offset
            .checked_add(out.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            return 0;
        }
        let Ok(mut file) = self.file.lock() else {
            return 0;
        };
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(out))
            .map(|_| out.len())
            .unwrap_or(0)
    }
}
fn usage() -> &'static str {
    "usage: plusdrive build SAMPLES -o NEW_IMAGE [--syx FILE] [--no-project] [--capacity-blocks N]\n       plusdrive ls IMAGE\n\nProject seeding requires a verified DT2 1.16 SysEx; without --syx the image is sample-only."
}
fn build(args: Vec<String>) -> Result<(), String> {
    let mut source = None;
    let mut output = None;
    let mut capacity = 0x760000u32;
    let mut syx = None;
    let mut no_project = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" => output = Some(PathBuf::from(it.next().ok_or("missing -o value")?)),
            "--capacity-blocks" => {
                capacity = it
                    .next()
                    .ok_or("missing --capacity-blocks value")?
                    .parse()
                    .map_err(|_| "invalid --capacity-blocks")?
            }
            "--syx" => syx = Some(PathBuf::from(it.next().ok_or("missing --syx value")?)),
            "--no-project" => no_project = true,
            _ if source.is_none() => source = Some(PathBuf::from(arg)),
            _ => return Err(usage().into()),
        }
    }
    let source = source.ok_or(usage())?;
    let output = output.ok_or(usage())?;
    if no_project && syx.is_some() {
        return Err("--syx and --no-project cannot be combined".into());
    }
    if output.exists() {
        return Err("output already exists".into());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_file()
            && path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("wav"))
        {
            paths.push((entry.file_name(), path));
        }
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    if paths.is_empty() {
        return Err("no direct .wav files".into());
    }
    if paths.len() > 2048 {
        return Err("too many samples".into());
    }
    let mut samples = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for (name, path) in paths {
        let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
        if meta.len() > 64 * 1024 * 1024 {
            return Err(format!("{} exceeds WAV source limit", path.display()));
        };
        let wav = fs::read(&path).map_err(|e| e.to_string())?;
        let sample = wav_to_native(&wav, WavLimits::default())
            .map_err(|e| format!("{}: {e:?}", path.display()))?;
        total = total
            .checked_add(sample.bytes.len())
            .ok_or("native sample size overflow")?;
        if total > 256 * 1024 * 1024 {
            return Err("native sample aggregate exceeds limit".into());
        };
        samples.push(NamedSample {
            file_name: name
                .into_string()
                .map_err(|_| "sample file name is not UTF-8")?,
            sample,
        });
    }
    let main;
    let seed = if let Some(syx) = syx {
        let bytes = fs::read(&syx).map_err(|e| format!("{}: {e}", syx.display()))?;
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let firmware =
            dt2_firmware_loader::parse(&bytes).map_err(|e| format!("{}: {e}", syx.display()))?;
        main = firmware
            .sections
            .into_iter()
            .find(|section| section.id == 3)
            .ok_or("firmware has no MAIN section")?
            .bytes;
        let registry = Registry::embedded().map_err(|e| e.to_string())?;
        let (device, profile, _) = registry
            .boot_for_main(&digest, &main)
            .map_err(|e| e.to_string())?;
        if device.short != "dt2" {
            return Err("project seeding is DT2-only".into());
        }
        Some(ProjectSeed {
            main: &main,
            contract: profile
                .plusdrive_project_contract
                .ok_or("firmware has no +Drive project contract")?,
        })
    } else {
        None
    };
    let built = build_sample_image_with_options(samples, capacity, seed)
        .map_err(|e| format!("build: {e:?}"))?;
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|e| e.to_string())?;
        file.set_len(built.image.logical_len())
            .map_err(|e| e.to_string())?;
        let mut bytes = 0usize;
        for (&at, data) in built.image.ranges() {
            file.seek(SeekFrom::Start(at))
                .and_then(|_| file.write_all(data))
                .map_err(|e| e.to_string())?;
            bytes += data.len()
        }
        file.flush().map_err(|e| e.to_string())?;
        println!(
            "logical_bytes={} written_bytes={} sample_entries={} project={:?}",
            built.image.logical_len(),
            bytes,
            built.files.len(),
            built.project
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&output);
    };
    result
}
fn ls(path: PathBuf) -> Result<(), String> {
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() || meta.len() % 512 != 0 {
        return Err("image must be a regular 512-byte-aligned file".into());
    };
    let image = FileImage {
        file: Mutex::new(fs::File::open(path).map_err(|e| e.to_string())?),
        len: meta.len(),
    };
    for entry in list_image(&image).map_err(|e| format!("invalid image: {e:?}"))? {
        println!(
            "{}\t{}\t{}\t{}",
            entry.id,
            String::from_utf8_lossy(&entry.name),
            entry.kind,
            entry.size
        )
    }
    Ok(())
}
fn main() {
    let mut args = env::args().skip(1);
    let command = args.next();
    let rest = args.collect();
    let result = match command.as_deref() {
        Some("build") => build(rest),
        Some("ls") => {
            if rest.len() == 1 {
                ls(PathBuf::from(&rest[0]))
            } else {
                Err(usage().into())
            }
        }
        Some("--help") | Some("-h") => {
            println!("{}", usage());
            Ok(())
        }
        _ => Err(usage().into()),
    };
    if let Err(e) = result {
        eprintln!("plusdrive: {e}");
        std::process::exit(2)
    }
}
