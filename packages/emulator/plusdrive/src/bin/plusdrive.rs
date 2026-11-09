use device_profile::Registry;
use emmc_card::RandomAccessRead;
use plusdrive_format::{
    EntryKind, Fs, ProjectSeed, TreeOptions, build_tree_image, card_layout, export::with_overlay,
    list_image, read_store, read_store_file, scan_folder, write_image_file,
};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn usage() -> &'static str {
    "usage: plusdrive build DIR -o NEW_IMAGE [--device dt2|dn2] [--syx FILE] [--no-project] [--capacity-blocks N] [--max-extent-pages N]\n       plusdrive ls IMAGE\n       plusdrive fs ls IMAGE [--overlay FILE] | fs cat IMAGE PATH [--overlay FILE]\n       plusdrive store ls IMAGE [--overlay FILE] [--device dt2|dn2] [--cat PATH]\n       plusdrive export IMAGE [--overlay FILE] -o FOLDER [--out DIR] [--prune] [--with-working-project] [--device dt2|dn2]\n\nDIR is a plus_drive/<device> folder: files and folders map to the path filesystem (a .wav becomes a native\nsample named without the extension; any other file is copied byte for byte), .mmcfs/{projects,sounds,kits}/NNNN.bin\nare raw MmcFs slots, .mmcfs/boot/SSSSSS.bin are the project regions outside the pools (a boot copy replaces the\ngenerated boot project) and digikit/ goes to the module store (docs/design/digikit-store.md). A device without a\npath filesystem (DN2) takes only .mmcfs and digikit.\nThe card layout of --device (default dt2) comes from devices/*.toml [card].\nA boot project is seeded from a verified DT2 1.16 SysEx (--syx) when the folder holds a native sample."
}

fn build(args: Vec<String>) -> Result<(), String> {
    let mut source = None;
    let mut output = None;
    let mut capacity = 0x760000u32;
    let mut syx = None;
    let mut no_project = false;
    let mut device = None;
    let mut max_extent_pages = u32::MAX;
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
            "--max-extent-pages" => {
                max_extent_pages = it
                    .next()
                    .ok_or("missing --max-extent-pages value")?
                    .parse()
                    .map_err(|_| "invalid --max-extent-pages")?
            }
            "--device" => device = Some(it.next().ok_or("missing --device value")?),
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
    let main;
    let mut seed = None;
    let mut device_short = device;
    if let Some(syx) = &syx {
        let bytes = fs::read(syx).map_err(|e| format!("{}: {e}", syx.display()))?;
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
        let registry = Registry::load().map_err(|e| e.to_string())?;
        let (found, profile, _) = registry
            .boot_for_main(&digest, &main)
            .map_err(|e| e.to_string())?;
        if device_short.as_deref().is_some_and(|d| d != found.short) {
            return Err(format!("--device does not match {}", syx.display()));
        }
        device_short = Some(found.short.to_string());
        if found.short == "dt2" {
            seed = Some(ProjectSeed {
                main: &main,
                contract: profile
                    .plusdrive_project_contract
                    .ok_or("firmware has no +Drive project contract")?,
            });
        }
    }
    let device = device_short.unwrap_or_else(|| "dt2".into());
    let layout = card_layout(&device)?;
    let scanned = scan_folder(&source, &layout, &mut |_, _| {})?;
    for line in &scanned.skipped {
        eprintln!("plusdrive: skipped {line}");
    }
    let slot_count = scanned.tree.slots.slots.len();
    let region_count = scanned.tree.regions.len();
    let store_count = scanned.tree.store.len();
    let built = build_tree_image(
        scanned.tree,
        TreeOptions {
            capacity_blocks: capacity,
            layout,
            max_extent_pages,
        },
        if no_project { None } else { seed },
    )
    .map_err(|e| format!("build: {e:?}"))?;
    let (mut dirs, mut plain) = (0usize, 0usize);
    for entry in &built.entries {
        match entry.kind {
            EntryKind::Dir => dirs += 1,
            EntryKind::Plain => plain += 1,
            EntryKind::Native => {}
        }
    }
    let written = write_image_file(&built.image, &output)?;
    println!(
        "logical_bytes={} written_bytes={} sample_entries={} plain_files={} directories={} mmcfs_slots={} boot_regions={} store_files={} project={:?}",
        built.image.logical_len(),
        written,
        built.files.len(),
        plain,
        dirs.saturating_sub(1),
        slot_count,
        region_count,
        store_count,
        built.project
    );
    Ok(())
}
/// Remove `--overlay FILE` from `args`: a `card.overlay` file of the guest's writes to lay over the image.
fn take_overlay(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let Some(at) = args.iter().position(|a| a == "--overlay") else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err("missing --overlay value".into());
    }
    let value = args.remove(at + 1);
    args.remove(at);
    Ok(Some(PathBuf::from(value)))
}
/// Run `f` on the image seen through the overlay.
fn with_image<T>(
    path: &PathBuf,
    overlay: Option<PathBuf>,
    f: impl FnOnce(&dyn RandomAccessRead) -> Result<T, String>,
) -> Result<T, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() || meta.len() % 512 != 0 {
        return Err("image must be a regular 512-byte-aligned file".into());
    };
    with_overlay(path, overlay.as_deref(), f)
}
/// `plusdrive store ls IMAGE [--overlay FILE] [--device D] [--cat PATH]`: the module store's files (path, first sector,
/// bytes, file hash slot), or one file's bytes on stdout.
fn store_ls(mut args: Vec<String>) -> Result<(), String> {
    let overlay = take_overlay(&mut args)?;
    let (mut image, mut device, mut cat) = (None, String::from("dt2"), None);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--device" => device = it.next().ok_or("missing --device value")?,
            "--cat" => cat = Some(it.next().ok_or("missing --cat value")?),
            _ if image.is_none() && !arg.starts_with('-') => image = Some(PathBuf::from(arg)),
            _ => return Err(usage().into()),
        }
    }
    let layout = card_layout(&device)?;
    let range = layout.store.ok_or("this device has no module store")?;
    let size = range.store_limit();
    with_image(&image.ok_or(usage())?, overlay, |image| {
        let store = read_store(image, range.first_sector, size, range.header_b())
            .map_err(|e| e.to_string())?
            .ok_or("no module store on this card")?;
        if let Some(path) = cat {
            let entry = store
                .entries
                .iter()
                .find(|e| e.path == path)
                .ok_or_else(|| format!("digikit/{path}: not in the store"))?;
            let bytes =
                read_store_file(image, range.first_sector, entry).map_err(|e| e.to_string())?;
            use std::io::Write as _;
            return std::io::stdout()
                .write_all(&bytes)
                .map_err(|e| e.to_string());
        }
        println!(
            "# used_sectors={} of {size} version={} seq={} header={}",
            store.used_sectors,
            store.version,
            store.seq,
            if store.from_b { "B" } else { "A" }
        );
        // by path (the table is by position)
        let mut entries: Vec<_> = store.entries.iter().collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for e in entries {
            let hex: String = e.hash.iter().map(|b| format!("{b:02x}")).collect();
            if e.is_dir {
                println!("{}/\t-\t0\tfolder", e.path);
            } else {
                println!("{}\t{}\t{}\t{hex}", e.path, e.first_sector, e.size_bytes);
            }
        }
        Ok(())
    })
}
fn ls(path: PathBuf) -> Result<(), String> {
    with_image(&path, None, |image| {
        for entry in list_image(image).map_err(|e| format!("invalid image: {e:?}"))? {
            println!(
                "{}\t{}\t{}\t{}",
                entry.id,
                String::from_utf8_lossy(&entry.name),
                entry.kind,
                entry.size
            )
        }
        Ok(())
    })
}
/// `plusdrive fs ls IMAGE [--overlay FILE]`: the path filesystem (the +Drive) as `path`, size and `hash|1` (a folder: `/`);
/// `plusdrive fs cat IMAGE PATH [--overlay FILE]`: one file's bytes on stdout, the 16-byte trailer included (what the card
/// holds; a RPC listing shows the size without it).
fn fs_cmd(mut args: Vec<String>) -> Result<(), String> {
    let overlay = take_overlay(&mut args)?;
    let (verb, image, path) = match args.as_slice() {
        [v, i] if v == "ls" => (v, i, None),
        [v, i, p] if v == "cat" => (v, i, Some(p.trim_start_matches('/').to_owned())),
        _ => return Err(usage().into()),
    };
    with_image(&PathBuf::from(image), overlay, |image| {
        let fs = Fs::open(image).map_err(|e| format!("no path filesystem: {e:?}"))?;
        let walk = fs.walk();
        let mut issues = Vec::new();
        for node in &walk.nodes {
            let name = String::from_utf8_lossy(&node.path).into_owned();
            if verb == "ls" {
                if node.record.is_dir() {
                    println!("{name}/\t0\t-");
                } else {
                    println!("{name}\t{}\t{:08x}", node.record.size, node.record.hash);
                }
            } else if Some(&name) == path.as_ref() && !node.record.is_dir() {
                let bytes = fs.read_content(&node.record, &mut issues)?;
                use std::io::Write as _;
                return std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string());
            }
        }
        if verb == "cat" {
            return Err(format!(
                "{}: not in the path filesystem",
                path.unwrap_or_default()
            ));
        }
        Ok(())
    })
}
fn main() {
    let mut args = env::args().skip(1);
    let command = args.next();
    let rest = args.collect();
    let result = match command.as_deref() {
        Some("build") => build(rest),
        Some("export") => plusdrive_format::export::cli(rest),
        Some("store") if rest.first().map(String::as_str) == Some("ls") => {
            store_ls(rest[1..].to_vec())
        }
        Some("fs") => fs_cmd(rest),
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
