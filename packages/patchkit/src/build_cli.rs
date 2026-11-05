//! The command line of `patchkit build`, `patchkit dko show` and `patchkit cf-link` (the library is `patchkit::build`).

use patchkit::build::{self, BuildOptions, cf, show};
use patchkit::dko::Dko;
use patchkit::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn flag(rest: &[String], name: &str) -> Option<String> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1).cloned())
}

fn flags(rest: &[String], name: &str) -> Vec<String> {
    rest.iter()
        .enumerate()
        .filter(|(_, a)| *a == name)
        .filter_map(|(i, _)| rest.get(i + 1).cloned())
        .collect()
}

/// The arguments that are not flags or the values of flags that take one.
fn positional(rest: &[String], valued: &[&str]) -> Vec<String> {
    let mut v = vec![];
    let mut i = 0;
    while i < rest.len() {
        if valued.contains(&rest[i].as_str()) {
            i += 2;
        } else if rest[i].starts_with('-') {
            i += 1;
        } else {
            v.push(rest[i].clone());
            i += 1;
        }
    }
    v
}

pub fn build(root: &Path, rest: &[String]) -> Result<()> {
    const KNOWN: [&str; 8] = [
        "--check", "--force", "--define", "--out", "--status", "--tree", "--all", "--",
    ];
    if let Some(f) = rest
        .iter()
        .find(|a| a.starts_with('-') && !KNOWN.contains(&a.as_str()))
    {
        return Err(Error(format!("build: unknown option {f}")));
    }
    let valued = ["--define", "--out", "--tree"];
    let args = positional(rest, &valued);
    let has = |f: &str| rest.iter().any(|a| a == f);
    let tree = flag(rest, "--tree").map(PathBuf::from);
    // (the name the caller used, the module directory)
    let dirs: Vec<(String, PathBuf)> = if args.is_empty() {
        build::all_modules(root, tree.as_deref())
            .into_iter()
            .map(|d| (d.display().to_string(), d))
            .collect()
    } else {
        args.iter()
            .map(|a| Ok((a.clone(), build::resolve_module(root, a)?)))
            .collect::<Result<_>>()?
    };
    if has("--status") {
        let mut m = serde_json::Map::new();
        for (name, d) in &dirs {
            let ok = build::module_status(root, d)
                .map(|v| v.iter().all(|x| x.1))
                .unwrap_or(false);
            m.insert(
                name.clone(),
                if ok { "unchanged" } else { "needs build" }.into(),
            );
        }
        println!("{}", serde_json::Value::Object(m));
        return Ok(());
    }
    let opts = BuildOptions {
        root: root.to_path_buf(),
        check: has("--check"),
        force: has("--force"),
        defines: flags(rest, "--define")
            .into_iter()
            .map(|d| d.split('=').next().unwrap_or("").to_string())
            .collect(),
        out: flag(rest, "--out").map(PathBuf::from),
    };
    for (_, d) in dirs {
        for line in build::build_module(&d, &opts)?.lines() {
            println!("{line}");
        }
    }
    Ok(())
}

/// The instructions of one `.dko.json` (ColdFire), ELF object or `ar` archive, checked against the ColdFire V4e ISA; `None` for an
/// object that is not ColdFire.
fn isa_check_file(file: &str) -> Result<Option<u32>> {
    let name = Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if file.ends_with(".json") {
        let o =
            Dko::parse(&std::fs::read_to_string(file).map_err(|e| Error(format!("{file}: {e}")))?)?;
        if o.arch != "cf" {
            return Ok(None);
        }
        let mut secs = vec![];
        for s in o.sections.iter().filter(|s| s.kind == "text") {
            secs.push((s.name.clone(), patchkit::error::unhex(&s.hex)?));
        }
        let hist = cf::check_text(&secs, &format!("{name}: "))?;
        return Ok(Some(hist.values().sum()));
    }
    let blob = std::fs::read(file).map_err(|e| Error(format!("{file}: {e}")))?;
    let members = if blob.starts_with(b"!<arch>\n") {
        build::elf::ar_members(&blob)?
    } else {
        vec![(name.clone(), blob)]
    };
    let (mut total, mut bad) = (0u32, vec![]);
    for (member, body) in members {
        let checked = build::elf::text_sections(&body)
            .and_then(|secs| cf::check_text(&secs, &format!("{name}: {member}:")));
        match checked {
            Ok(h) => total += h.values().sum::<u32>(),
            Err(e) => bad.push(
                e.0.split_once('\n')
                    .map_or(e.0.clone(), |x| x.1.trim().to_string()),
            ),
        }
    }
    if !bad.is_empty() {
        return Err(Error(format!(
            "instructions outside ISA_A/B/C+EMAC:\n  {}",
            bad.join("\n  ")
        )));
    }
    Ok(Some(total))
}

pub fn dko(rest: &[String]) -> Result<()> {
    const U: &str = "usage: patchkit dko show FILE.dko.json [--relocs] | patchkit dko isa-digest FILE.dko.json | patchkit dko isa-check FILE...";
    let cmd = rest.first().map(String::as_str);
    if !matches!(cmd, Some("show" | "isa-digest" | "isa-check")) {
        return Err(Error(U.into()));
    }
    let files = positional(&rest[1..], &[]);
    if files.is_empty() {
        return Err(Error(U.into()));
    }
    if cmd == Some("isa-check") {
        // the ColdFire ISA allow-list lives in `build::cf` only; one line per file, a failure at the end
        let mut failed = false;
        for f in &files {
            match isa_check_file(f) {
                Ok(Some(n)) => println!("ok   {f}: {n} instructions"),
                Ok(None) => println!("ok   {f}: no cf code"),
                Err(e) => {
                    failed = true;
                    println!("FAIL {f}\n  {e}");
                }
            }
        }
        return if failed {
            Err(Error("ISA check failed".into()))
        } else {
            Ok(())
        };
    }
    let file = &files[0];
    let text = std::fs::read_to_string(file).map_err(|e| Error(format!("{file}: {e}")))?;
    let o = Dko::parse(&text)?;
    if cmd == Some("isa-digest") {
        println!("{}", o.isa_digest()?);
    } else {
        print!(
            "{}",
            show::summary(&o, rest.iter().any(|a| a == "--relocs"))
        );
    }
    Ok(())
}

fn hex_u32(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|_| Error(format!("bad hex number {s:?}")))
}

pub fn cf_link(root: &Path, rest: &[String]) -> Result<()> {
    let sources: Vec<PathBuf> = positional(rest, &["--base", "--sym", "-I", "--bin", "--json"])
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if sources.is_empty() {
        return Err(Error("cf-link needs sources".into()));
    }
    let base =
        hex_u32(&flag(rest, "--base").ok_or_else(|| Error("cf-link needs --base HEX".into()))?)?;
    let mut syms: HashMap<String, u32> = HashMap::new();
    for s in flags(rest, "--sym") {
        let (n, v) = s
            .split_once('=')
            .ok_or_else(|| Error(format!("--sym NAME=HEX, got {s:?}")))?;
        syms.insert(n.to_string(), hex_u32(v)?);
    }
    let inc: Vec<PathBuf> = flags(rest, "-I").into_iter().map(PathBuf::from).collect();
    let (l, hist) = build::link_sources(root, &sources, base, &syms, &inc)?;
    if let Some(p) = flag(rest, "--bin") {
        std::fs::write(&p, &l.image).map_err(|e| Error(format!("{p}: {e}")))?;
    }
    let j = serde_json::json!({
        "base": l.base,
        "bss_size": l.bss_size,
        "symbols": l.symbols.iter().collect::<std::collections::BTreeMap<_, _>>(),
        "text_ranges": l.text_ranges,
        "relocs_applied": l.relocs_applied,
        "histogram": hist,
        "instructions": hist.values().sum::<u32>(),
    });
    match flag(rest, "--json") {
        Some(p) => std::fs::write(&p, j.to_string()).map_err(|e| Error(format!("{p}: {e}")))?,
        None => println!("{j}"),
    }
    let _ = cf::BASE;
    Ok(())
}

/// `patchkit sharc`: the SHARC+ build for the Python test rigs (they place an object themselves and run it on the core).
///
/// * `text [--module DIR] [-I DIR]... [--define NAME]... SRC...`: the expanded source text (JSON-free, on stdout).
/// * `cond [--define NAME]...`: stdin with the `#ifdef` alternatives selected, on stdout. `inc [-I DIR]...`: stdin with the `#include`s expanded.
/// * `object INPUT`: the `.dko.json` object (what `patchkit build` writes).
/// * `asm INPUT`: the assembled object as JSON (`build::sharc_obj::object_json`).
/// * `link INPUT --base N [--extern NAME=ADDR]...`: the object placed at the short-word address, verified; JSON.
/// * `shift-check FILE`: for each line of hex 48-bit instruction words, the shift-immediate problem or `ok`.
///
/// INPUT is `--text FILE` (an expanded source; `-` is stdin) or `--elf FILE.doj --source FILE` (an object another Selache
/// tool wrote, with its source for the hash and the assembly it was made from for the encoding-bug guards).
pub fn sharc(root: &Path, rest: &[String]) -> Result<()> {
    use build::sharc as bs;
    use build::sharc_obj as so;
    const U: &str = "usage: patchkit sharc text [--module DIR] [-I DIR]... [--define NAME]... SRC... | cond [--define NAME]... | inc [-I DIR]... | object INPUT | asm INPUT | link INPUT --base N [--extern NAME=ADDR]... | shift-check FILE\n  INPUT: --text FILE | --elf FILE.doj --source FILE";
    let cmd = rest.first().map(String::as_str).unwrap_or("");
    let rest = rest.get(1..).unwrap_or(&[]);
    let read = |p: &str| -> Result<String> {
        if p == "-" {
            let mut t = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)
                .map_err(|e| Error(format!("stdin: {e}")))?;
            Ok(t)
        } else {
            std::fs::read_to_string(p).map_err(|e| Error(format!("{p}: {e}")))
        }
    };
    let num = |s: &str| -> Result<i64> {
        match s.strip_prefix("0x") {
            Some(h) => i64::from_str_radix(h, 16),
            None => s.parse(),
        }
        .map_err(|_| Error(format!("bad number {s:?}")))
    };
    let object = || -> Result<so::Obj> {
        if let Some(t) = flag(rest, "--text") {
            return so::assemble(&read(&t)?);
        }
        let elf = flag(rest, "--elf").ok_or_else(|| Error(U.into()))?;
        let src = read(&flag(rest, "--source").ok_or_else(|| Error(U.into()))?)?;
        let blob = std::fs::read(&elf).map_err(|e| Error(format!("{elf}: {e}")))?;
        so::object_from_elf(&blob, &src)
    };
    match cmd {
        "text" => {
            let srcs: Vec<PathBuf> = positional(rest, &["--module", "-I", "--define"])
                .into_iter()
                .map(PathBuf::from)
                .collect();
            let mut dirs = match flag(rest, "--module") {
                Some(m) => bs::include_dirs(&build::toolchain::canon(root), Path::new(&m)),
                None => vec![],
            };
            dirs.extend(flags(rest, "-I").into_iter().map(PathBuf::from));
            let defines = flags(rest, "--define");
            print!("{}", bs::source_text_in(dirs, &srcs, &defines)?);
        }
        "cond" => print!(
            "{}",
            bs::apply_conditionals(&read("-")?, &flags(rest, "--define"))?
        ),
        "inc" => {
            let dirs: Vec<PathBuf> = flags(rest, "-I").into_iter().map(PathBuf::from).collect();
            print!("{}", bs::expand_includes(&read("-")?, &dirs, 0)?);
        }
        "object" => {
            let o = match flag(rest, "--text") {
                Some(t) => so::object_from_text(&read(&t)?)?,
                None => return Err(Error(U.into())),
            };
            print!("{}", build::to_text(&o)?);
        }
        "asm" => println!("{}", so::object_json(&object()?)),
        "link" => {
            let base = num(&flag(rest, "--base").ok_or_else(|| Error(U.into()))?)?;
            let mut externs = HashMap::new();
            for e in flags(rest, "--extern") {
                let (n, v) = e
                    .split_once('=')
                    .ok_or_else(|| Error(format!("--extern NAME=ADDR, got {e:?}")))?;
                externs.insert(n.to_string(), num(v)?);
            }
            println!("{}", so::link_json(&object()?, base, &externs)?);
        }
        "shift-check" => {
            let text = read(rest.first().ok_or_else(|| Error(U.into()))?)?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let w = u64::from_str_radix(line.trim(), 16)
                    .map_err(|_| Error(format!("bad hex word {line:?}")))?;
                println!(
                    "{}",
                    so::shift_problem_of_word(w).unwrap_or_else(|| "ok".into())
                );
            }
        }
        _ => return Err(Error(U.into())),
    }
    Ok(())
}
