//! Rust crates for the ColdFire: crate -> one relocatable ELF -> an object prototype.
//!
//! The crate is `#![no_std]`, `panic = "abort"`, `crate-type = ["staticlib"]` and has its own `[workspace]`. It is built with the GCC
//! back end for -mcpu=54418 (rustc_codegen_gcc, `pk_toolchains codegen-gcc`) and `-Zbuild-std=core,compiler_builtins` with the `mem`
//! feature: `core`, the integer helpers (`__muldi3`, `__udivdi3`, ...) and `memcpy`/`memset`/`memcmp` come from Rust sources compiled
//! for the ColdFire, not from the m68k-elf libgcc (68020 code). The staticlib is combined with `ld -r --gc-sections`, keeping the
//! crate's own global symbols, so only what they reach stays (docs/findings/26-rust-on-coldfire.md).

use super::elf::{self, Proto};
use super::toolchain::{self as tc, Env, TempDir};
use crate::error::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Target helpers are source-hashed so fixes invalidate cached ColdFire objects.
const CF_HELPERS: &str = include_str!("cf_rust_helpers.s");

/// Emitted into the object, so identical on every machine: no source paths in panic locations.
const RUSTFLAGS_EXTRA: &str = "-Zlocation-detail=none -Zfmt-debug=none";
const CARGO_ARGS: [&str; 3] = [
    "-Zbuild-std=core,compiler_builtins",
    "-Zbuild-std-features=compiler-builtins-mem",
    "-Zjson-target-spec",
];
const EF_M68K_CF_ISA_MASK: u32 = 0x0F;
const EF_M68K_CF_ISA_C: u32 = 6;
const EF_M68K_CF_MAC_MASK: u32 = 0x30;
const EF_M68K_CF_EMAC: u32 = 0x20;

#[derive(Deserialize)]
struct Cargo {
    package: Package,
    lib: Option<Lib>,
    profile: Option<Profile>,
    dependencies: Option<BTreeMap<String, toml::Spanned<toml::Value>>>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

#[derive(Deserialize)]
struct Lib {
    #[serde(rename = "crate-type", default)]
    crate_type: Vec<String>,
}

#[derive(Deserialize)]
struct Profile {
    release: Option<Release>,
}

#[derive(Deserialize)]
struct Release {
    panic: Option<String>,
}

fn parse_cargo(text: &str, what: &Path) -> Result<Cargo> {
    toml::from_str(text).map_err(|e| err!("{}: {e}", what.display()))
}

/// The `path` of every path dependency of a manifest, in the order the file lists them.
fn path_deps(text: &str, what: &Path) -> Result<Vec<String>> {
    let c = parse_cargo(text, what)?;
    let mut deps: Vec<(usize, String)> = vec![];
    for (_, d) in c.dependencies.unwrap_or_default() {
        if let Some(p) = d.get_ref().get("path").and_then(|p| p.as_str()) {
            deps.push((d.span().start, p.to_string()));
        }
    }
    deps.sort();
    Ok(deps.into_iter().map(|d| d.1).collect())
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `os.path.relpath`: both paths absolute and normalised.
pub fn relpath(p: &Path, start: &Path) -> String {
    let a: Vec<Component> = p.components().collect();
    let b: Vec<Component> = start.components().collect();
    let n = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec![];
    for _ in n..b.len() {
        parts.push("..".into());
    }
    for c in &a[n..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// The crate's manifest and sources, and those of its path dependencies (for example `dkrt-sdk`).
pub fn crate_files(manifest: &Path) -> Result<Vec<PathBuf>> {
    let d = manifest.parent().unwrap_or(Path::new("."));
    let mut out = vec![manifest.to_path_buf()];
    let mut rs = vec![];
    rs_files(&d.join("src"), &mut rs);
    rs.sort();
    out.extend(rs);
    for extra in ["build.rs", "rust-toolchain.toml"] {
        if d.join(extra).exists() {
            out.push(d.join(extra));
        }
    }
    for dep in path_deps(&tc::read_text(manifest)?, manifest)? {
        let sub = tc::canon(&d.join(&dep).join("Cargo.toml"));
        for p in crate_files(&sub)? {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// The crate's sources (relative names and bytes) and the toolchain pins: what a build depends on.
pub fn source_sha256(manifest: &Path) -> Result<String> {
    let manifest = tc::canon(manifest);
    let base = manifest.parent().unwrap_or(Path::new("."));
    let mut h = Sha256::new();
    for p in crate_files(&manifest)? {
        h.update(relpath(&p, base).as_bytes());
        h.update([0u8]);
        h.update(tc::read(&p)?);
    }
    let pins = tc::pins();
    h.update(
        [
            pins.rust_nightly.as_str(),
            pins.cg_rev.as_str(),
            pins.gcc_jit_rev.as_str(),
            RUSTFLAGS_EXTRA,
        ]
        .join("\0")
        .as_bytes(),
    );
    h.update(tc::RUST_TARGET_JSON);
    h.update(CF_HELPERS.as_bytes());
    Ok(hex(&h.finalize()))
}

fn toolchain_info(env: &Env) -> BTreeMap<String, String> {
    let pins = tc::pins();
    let rustc = std::process::Command::new("rustup")
        .arg("run")
        .arg(&pins.rust_nightly)
        .args(["rustc", "--version"])
        .env_clear()
        .envs(env.codegen_gcc_env(""))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    BTreeMap::from([
        ("rustc".into(), rustc),
        ("rustc_codegen_gcc".into(), pins.cg_rev.clone()),
        ("libgccjit_gcc".into(), pins.gcc_jit_rev.clone()),
        (
            "rustflags".into(),
            format!("-Cllvm-args=-mcpu=54418 -Cpanic=abort {RUSTFLAGS_EXTRA}"),
        ),
        ("target".into(), tc::RUST_TARGET_NAME.into()),
    ])
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| err!("{}: {e}", dst.display()))?;
    let rd = std::fs::read_dir(src).map_err(|e| err!("{}: {e}", src.display()))?;
    for e in rd.flatten() {
        let name = e.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let from = e.path();
        let to = dst.join(&name);
        let md = std::fs::metadata(&from).map_err(|e| err!("{}: {e}", from.display()))?;
        if md.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| err!("{}: {e}", from.display()))?;
            // The target dir is shared by every tree and Cargo compares modification times: stamp the copy now.
            std::fs::File::options()
                .write(true)
                .open(&to)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()))
                .map_err(|e| err!("{}: {e}", to.display()))?;
        }
    }
    Ok(())
}

/// `re.sub(r'(path\s*=\s*)"<p>"', r'\1"<rel>"', text)`.
fn replace_path(text: &str, p: &str, rel: &str) -> String {
    let quoted = format!("\"{p}\"");
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(after) = rest.strip_prefix("path") {
            let t1 = after.trim_start();
            if let Some(t2) = t1.strip_prefix('=') {
                let t3 = t2.trim_start();
                if t3.starts_with(&quoted) {
                    let consumed = rest.len() - t3.len() + quoted.len();
                    out.push_str(&rest[..consumed - quoted.len()]);
                    out.push_str(&format!("\"{rel}\""));
                    i += consumed;
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Copy a crate to `dest` and its path dependencies to `deps_dir/<n>-<name>`, rewriting the `path = ".."` of the copies.
fn stage(
    manifest: &Path,
    dest: &Path,
    deps_dir: &Path,
    seen: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<PathBuf> {
    copy_tree(manifest.parent().unwrap(), dest)?;
    let mut text = tc::read_text(&dest.join("Cargo.toml"))?;
    for dep in path_deps(&text.clone(), manifest)? {
        let sub = tc::canon(&manifest.parent().unwrap().join(&dep).join("Cargo.toml"));
        let sd = match seen.iter().find(|(k, _)| *k == sub) {
            Some((_, sd)) => sd.clone(),
            None => {
                let name = sub
                    .parent()
                    .and_then(|p| p.file_name())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let sd = deps_dir.join(format!("{}-{}", seen.len(), name));
                seen.push((sub.clone(), sd.clone()));
                stage(&sub, &sd, deps_dir, seen)?;
                sd
            }
        };
        text = replace_path(&text, &dep, &relpath(&sd, dest));
    }
    std::fs::write(dest.join("Cargo.toml"), text).map_err(|e| err!("{e}"))?;
    Ok(dest.join("Cargo.toml"))
}

/// The cargo target dir, shared by every module, worktree and lane that uses the same toolchain pin.
pub fn target_dir(env: &Env) -> PathBuf {
    let p = tc::pins();
    tc::canon(&env.root.join("out/cache/rust")).join(format!(
        "{}-cg{}-jit{}",
        p.rust_nightly,
        &p.cg_rev[..12],
        &p.gcc_jit_rev[..12]
    ))
}

/// Build the crate and copy its static library into `out_dir`. A lock on the shared target dir covers the cargo run and the copy, so a
/// build of the same crate from another worktree cannot replace the library in between.
fn build_staticlib(env: &Env, manifest: &Path, out_dir: &Path) -> Result<PathBuf> {
    if !env.codegen_gcc_ready() {
        return Err(err!(
            "Rust-for-ColdFire toolchain not installed (uv run python -m tools.patchkit.pk_toolchains codegen-gcc)"
        ));
    }
    let text = tc::read_text(manifest)?;
    let meta = parse_cargo(&text, manifest)?;
    let name = meta.package.name.replace('-', "_");
    if !meta
        .lib
        .as_ref()
        .is_some_and(|l| l.crate_type.iter().any(|t| t == "staticlib"))
    {
        return Err(err!(
            "{}: [lib] crate-type must be [\"staticlib\"]",
            manifest.display()
        ));
    }
    if meta
        .profile
        .and_then(|p| p.release)
        .and_then(|r| r.panic)
        .as_deref()
        != Some("abort")
    {
        return Err(err!(
            "{}: [profile.release] panic must be \"abort\"",
            manifest.display()
        ));
    }
    let pin = &tc::pins().rust_nightly;
    let rustc = std::process::Command::new("rustup")
        .args(["which", "--toolchain", pin.as_str(), "rustc"])
        .output()
        .map_err(|e| err!("rustup which rustc: {e}"))?;
    if !rustc.status.success() {
        return Err(err!(
            "cannot locate rustc for {pin}: {}",
            String::from_utf8_lossy(&rustc.stderr).trim()
        ));
    }
    let rustc = PathBuf::from(String::from_utf8_lossy(&rustc.stdout).trim());
    if !rustc.is_absolute() {
        return Err(err!(
            "rustup which returned no absolute rustc path for {pin}"
        ));
    }
    let tdir = target_dir(env);
    std::fs::create_dir_all(&tdir).map_err(|e| err!("{}: {e}", tdir.display()))?;
    let cg_env = env.codegen_gcc_env(RUSTFLAGS_EXTRA);
    let target = env
        .root
        .join("tools/patchkit/toolchain")
        .join(tc::RUST_TARGET_NAME);
    let lock = std::fs::File::create(tdir.join(".pk-rust.lock")).map_err(|e| err!("{e}"))?;
    lock.lock()
        .map_err(|e| err!("lock {}: {e}", tdir.display()))?;
    let src = TempDir::new("pk-rust-src-")?;
    let crate_dir = src.path().join("crate");
    let staged = stage(manifest, &crate_dir, &crate_dir.join(".deps"), &mut vec![])?;
    let mut cmd = std::process::Command::new("rustup");
    cmd.arg("run")
        .arg(&tc::pins().rust_nightly)
        .args(["cargo", "rustc", "--release"])
        .args(CARGO_ARGS)
        .arg("--target")
        .arg(&target)
        .arg("--target-dir")
        .arg(&tdir)
        .arg("--manifest-path")
        .arg(&staged)
        .current_dir(staged.parent().unwrap())
        .env_clear()
        .envs(cg_env)
        // Cargo otherwise finds the gate's real stable rustc on PATH, bypassing rustup's pin.
        .env("RUSTC", rustc);
    let o = cmd.output().map_err(|e| err!("rustup: {e}"))?;
    if !o.status.success() {
        let tail = |b: &[u8]| {
            let s = String::from_utf8_lossy(b).into_owned();
            let n = s.chars().count();
            s.chars().skip(n.saturating_sub(3000)).collect::<String>()
        };
        return Err(err!(
            "cargo failed:\n{}{}",
            tail(&o.stdout),
            tail(&o.stderr)
        ));
    }
    let stem = target.file_stem().unwrap().to_string_lossy().into_owned();
    let lib = tdir.join(stem).join("release").join(format!("lib{name}.a"));
    if !lib.exists() {
        return Err(err!("{} was not produced", lib.display()));
    }
    let dst = out_dir.join(lib.file_name().unwrap());
    std::fs::copy(&lib, &dst).map_err(|e| err!("{}: {e}", lib.display()))?;
    drop(lock);
    Ok(dst)
}

/// Names of the compiler's own helpers: `mem(cpy|move|set|cmp)`, `bcmp`, the integer and floating-point routines of libgcc, and the
/// 64/128-bit ones. `packages/patchkit/tests/build.rs` checks the rule against the symbols of the ColdFire libgcc archive.
pub fn is_helper(n: &str) -> bool {
    let word = |s: &str| s.chars().all(|c| c.is_alphanumeric() || c == '_');
    if let Some(r) = n.strip_prefix("mem")
        && matches!(r, "cpy" | "move" | "set" | "cmp")
    {
        return true;
    }
    if n == "bcmp" {
        return true;
    }
    let Some(r) = n.strip_prefix("__") else {
        return false;
    };
    const A: [&str; 17] = [
        "mul", "div", "udiv", "mod", "umod", "ashl", "lshr", "ashr", "clz", "ctz", "absv", "clrsb",
        "cmp", "ffs", "parity", "powi", "unord",
    ];
    if A.iter().any(|p| r.strip_prefix(p).is_some_and(word))
        || ["popcount", "bswap"]
            .iter()
            .any(|p| r.strip_prefix(p).is_some_and(word))
    {
        return true;
    }
    const B: [&str; 10] = [
        "add", "sub", "mul", "div", "neg", "eq", "ne", "lt", "le", "gt",
    ];
    const B2: [&str; 5] = ["ge", "float", "fix", "trunc", "ext"];
    for p in B.iter().chain(B2.iter()) {
        if let Some(rest) = r.strip_prefix(p)
            && word(rest)
        {
            // \w*[sd][fi]\d?
            let b = rest.as_bytes();
            let n = b.len();
            let is_sd_fi = |i: usize| {
                i + 1 < n && matches!(b[i], b's' | b'd') && matches!(b[i + 1], b'f' | b'i')
            };
            if (n >= 2 && is_sd_fi(n - 2))
                || (n >= 3 && b[n - 1].is_ascii_digit() && is_sd_fi(n - 3))
            {
                return true;
            }
        }
    }
    // __\w*(ti|di)\d
    let b = r.as_bytes();
    let n = b.len();
    word(r)
        && n >= 3
        && b[n - 1].is_ascii_digit()
        && (b[n - 3..n - 1] == *b"ti" || b[n - 3..n - 1] == *b"di")
}

/// (defined global symbols, undefined symbols) of an object, by `nm`.
fn globals(env: &Env, obj: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let args = vec!["--no-demangle".to_string(), obj.display().to_string()];
    let r = tc::run(&env.m68k("nm")?, &args, None)?;
    let (mut defined, mut undefined) = (vec![], vec![]);
    for line in r.stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 && parts[0] == "U" {
            undefined.push(parts[1].to_string());
        } else if parts.len() == 3 && "TDBRWVSA".contains(parts[1]) {
            defined.push(parts[2].to_string());
        }
    }
    Ok((defined, undefined))
}

fn check_flags(blob: &[u8], what: &str) -> Result<()> {
    let f = elf::e_flags(blob);
    if (f & EF_M68K_CF_ISA_MASK) != EF_M68K_CF_ISA_C || (f & EF_M68K_CF_MAC_MASK) != EF_M68K_CF_EMAC
    {
        return Err(err!(
            "{what}: ELF flags {f:#x} are not ColdFire ISA_C + EMAC (a 68020 or generic m68k object got in)"
        ));
    }
    Ok(())
}

/// Crate -> one relocatable ELF at `out`. Returns the exported symbols.
fn build_object(env: &Env, manifest: &Path, out: &Path) -> Result<Vec<String>> {
    let crate_name = parse_cargo(&tc::read_text(manifest)?, manifest)?
        .package
        .name
        .replace('-', "_");
    let work = TempDir::new("pk-rust-")?;
    let lib = build_staticlib(env, manifest, work.path())?;
    tc::run(
        &env.m68k("ar")?,
        &["x".to_string(), tc::canon(&lib).display().to_string()],
        Some(work.path()),
    )?;
    let mut members: Vec<PathBuf> = std::fs::read_dir(work.path())
        .map_err(|e| err!("{e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "o"))
        .collect();
    members.sort();
    let mut exports: Vec<String> = vec![];
    for m in &members {
        let blob = tc::read(m)?;
        if elf::is_empty(&blob)? {
            continue; // a crate with only types (dkrt-sdk) compiles to an object with no sections; its flags are 0
        }
        let mname = m.file_name().unwrap().to_string_lossy().into_owned();
        check_flags(&blob, &format!("archive member {mname}"))?;
        if mname.starts_with(&format!("{crate_name}-"))
            || mname.starts_with(&format!("{crate_name}."))
        {
            exports.extend(globals(env, m)?.0);
        }
    }
    if exports.is_empty() {
        return Err(err!(
            "{}: the crate exports no global symbol (#[no_mangle] pub ...)",
            manifest.display()
        ));
    }
    // Rust-mangled names (_R...) are internal (the panic handler, helpers with crate hashes): not roots
    exports.retain(|e| !e.starts_with("_R"));
    exports.sort();
    exports.dedup();
    let mut args = vec![
        "-r".to_string(),
        "--gc-sections".into(),
        "-o".into(),
        out.display().to_string(),
    ];
    for e in &exports {
        args.push("-u".into());
        args.push(e.clone());
    }
    // Strong ISA_C implementations override compiler_builtins' weak helpers.
    // They are not export roots: --gc-sections retains only referenced helpers,
    // and the usual localization pass below keeps them private to this object.
    let helper_src = work.path().join("cf-rust-helpers.s");
    let helper_obj = work.path().join("cf-rust-helpers.o");
    std::fs::write(&helper_src, CF_HELPERS).map_err(|e| err!("{e}"))?;
    tc::run(
        &env.m68k("as")?,
        &[
            "-mcpu=54418".into(),
            "-o".into(),
            helper_obj.display().to_string(),
            helper_src.display().to_string(),
        ],
        None,
    )?;
    args.push(helper_obj.display().to_string());
    args.push(tc::canon(&lib).display().to_string());
    tc::run(&env.m68k("ld")?, &args, None)?;
    let objcopy = env.m68k("objcopy")?;
    let o = out.display().to_string();
    // Internal names carry crate hashes: make them local, so the object does not depend on where the crate was built.
    tc::run(&objcopy, &tc::strs(&["-w", "-L", "_R*", &o]), None)?;
    // The compiler's own helpers are private copies: two Rust objects in one image would export the same name.
    let mut helpers: Vec<String> = globals(env, out)?
        .0
        .into_iter()
        .filter(|g| is_helper(g))
        .collect();
    helpers.sort();
    if !helpers.is_empty() {
        let mut args = vec![];
        for h in helpers {
            args.push("-L".to_string());
            args.push(h);
        }
        args.push(o.clone());
        tc::run(&objcopy, &args, None)?;
    }
    tc::run(
        &env.m68k("strip")?,
        &tc::strs(&["--strip-unneeded", &o]),
        None,
    )?;
    check_flags(&tc::read(out)?, &o)?;
    let mut missing: Vec<String> = globals(env, out)?
        .1
        .into_iter()
        .filter(|u| is_helper(u))
        .collect();
    missing.sort();
    if !missing.is_empty() {
        return Err(err!(
            "{}: compiler helper(s) left undefined: {} (they must come from compiler_builtins; the repo's libgcc is 68020 code and is not linked)",
            manifest.display(),
            missing.join(", ")
        ));
    }
    Ok(exports)
}

/// The prototype, source hash and toolchain record of a crate.
pub fn object_proto(
    env: &Env,
    manifest: &Path,
) -> Result<(Proto, String, BTreeMap<String, String>)> {
    let manifest = tc::canon(manifest);
    let tmp = TempDir::new("pk-rust-o-")?;
    let o = tmp.path().join("crate.o");
    let exports = build_object(env, &manifest, &o)?;
    let proto = elf::proto_from_elf(&tc::read(&o)?)?;
    for r in &proto.relocs {
        if r.rtype != "32" {
            return Err(err!(
                "relocation type R_68K_{} in {} is not supported in Rust modules (only R_68K_32; a PC-relative reference came from a codegen change)",
                r.rtype,
                r.section
            ));
        }
    }
    let mut toolchain = toolchain_info(env);
    toolchain.insert("exports".into(), exports.join(" "));
    Ok((proto, source_sha256(&manifest)?, toolchain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_tree_stamps_the_copy_with_the_current_time() {
        let src = TempDir::new("pk-copy-src-").unwrap();
        let dst = TempDir::new("pk-copy-dst-").unwrap();
        let file = src.path().join("lib.rs");
        std::fs::write(&file, b"fn f() {}").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        copy_tree(src.path(), dst.path()).unwrap();
        let copied = std::fs::metadata(dst.path().join("lib.rs"))
            .unwrap()
            .modified()
            .unwrap();
        assert!(copied > old);
    }
}
