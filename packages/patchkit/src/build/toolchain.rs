//! The pinned toolchains, found under `out/tools/` (tools/patchkit/pk_toolchains.py installs them), and child processes.

use crate::error::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// tools/patchkit/toolchain/pins.toml: the pins and the shared include directories.
const PINS_TOML: &str = include_str!("../../../../tools/patchkit/toolchain/pins.toml");
/// The Rust target of the ColdFire build (hashed into the source hash of a crate).
pub const RUST_TARGET_JSON: &[u8] =
    include_bytes!("../../../../tools/patchkit/toolchain/m68k-cf-none-elf-a2.json");
pub const RUST_TARGET_NAME: &str = "m68k-cf-none-elf-a2.json";

pub const CFLAGS: [&str; 15] = [
    "-mcpu=54418",
    "-Os",
    "-ffreestanding",
    "-fno-builtin",
    "-nostdlib",
    "-fno-pic",
    "-fno-pie",
    "-ffunction-sections",
    "-fdata-sections",
    "-fno-asynchronous-unwind-tables",
    "-fno-unwind-tables",
    "-fno-stack-protector",
    "-Wall",
    "-Wextra",
    "-Werror",
];
pub const ASFLAGS: [&str; 1] = ["-mcpu=54418"];
pub const LIBGCC_CF_NAME: &str = "libgcc-cf54418.a";

#[derive(Deserialize)]
pub struct Pins {
    pub rust_nightly: String,
    pub cg_rev: String,
    pub gcc_jit_rev: String,
    include: IncludeDirs,
}

#[derive(Deserialize)]
struct IncludeDirs {
    cf: Vec<String>,
    sharc: Vec<String>,
}

pub fn pins() -> &'static Pins {
    static P: OnceLock<Pins> = OnceLock::new();
    P.get_or_init(|| toml::from_str(PINS_TOML).expect("tools/patchkit/toolchain/pins.toml"))
}

/// The directories of the module-independent include path, relative to the repository root.
pub fn global_include_dirs(cf: bool) -> &'static [String] {
    if cf {
        &pins().include.cf
    } else {
        &pins().include.sharc
    }
}

/// The repository: where the tools and the caches are.
pub struct Env {
    pub root: PathBuf,
    /// `out/tools`, resolved (a lane's `out/` is a link to the main tree's).
    pub tools: PathBuf,
    /// Build defines (`DK_TRACE`): `-D` for the C sources, `#ifdef` names for the SHARC+ sources. They are part of the object's cache key.
    pub defines: Vec<String>,
    version: OnceLock<std::result::Result<Vec<(String, String)>, String>>,
}

pub fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Env {
    pub fn new(root: &Path) -> Env {
        let root = canon(root);
        let tools = canon(&root.join("out/tools"));
        Env {
            root,
            tools,
            defines: vec![],
            version: OnceLock::new(),
        }
    }

    /// The same environment building with `defines`.
    pub fn with_defines(mut self, defines: &[String]) -> Env {
        self.defines = defines.to_vec();
        self
    }

    pub fn m68k_prefix(&self) -> PathBuf {
        self.tools.join("m68k-elf")
    }

    pub fn m68k(&self, name: &str) -> Result<PathBuf> {
        let p = self
            .m68k_prefix()
            .join("bin")
            .join(format!("m68k-elf-{name}"));
        if p.exists() {
            Ok(p)
        } else {
            Err(err!(
                "m68k-elf toolchain not installed (uv run python -m tools.patchkit.pk_toolchains m68k)"
            ))
        }
    }

    pub fn libgcc_cf(&self) -> Option<PathBuf> {
        let p = self.m68k_prefix().join("lib").join(LIBGCC_CF_NAME);
        p.exists().then_some(p)
    }

    pub fn cg_dir(&self) -> PathBuf {
        self.tools.join("codegen-gcc")
    }

    pub fn codegen_backend(&self) -> PathBuf {
        // the back end is a shared library: .dylib on macOS, .so on Linux (pk_toolchains.codegen_gcc_backend)
        let lib = if cfg!(target_os = "macos") {
            "librustc_codegen_gcc.dylib"
        } else {
            "librustc_codegen_gcc.so"
        };
        self.cg_dir().join("rustc_codegen_gcc/target/debug").join(lib)
    }

    pub fn codegen_gcc_ready(&self) -> bool {
        self.codegen_backend().exists()
            && self.cg_dir().join("jitlib/libgccjit.so").exists()
            && self.m68k_prefix().join("bin/m68k-elf-as").exists()
    }

    /// Environment for the pinned nightly Cargo with the GCC back end for -mcpu=54418.
    pub fn codegen_gcc_env(&self, extra_rustflags: &str) -> Vec<(String, String)> {
        // The native gate pins stable compiler paths. Let the explicit nightly invocation select
        // its own compiler and documentation tool instead of inheriting those overrides.
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| !matches!(k.as_str(), "RUSTUP_TOOLCHAIN" | "RUSTC" | "RUSTDOC"))
            .collect();
        let path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{path}", self.m68k_prefix().join("bin").display());
        let flags = format!(
            "-Zcodegen-backend={} -Cllvm-args=-mcpu=54418 -Cpanic=abort {extra_rustflags}",
            self.codegen_backend().display()
        );
        let set = |env: &mut Vec<(String, String)>, k: &str, v: String| {
            env.retain(|(a, _)| a != k);
            env.push((k.into(), v));
        };
        set(&mut env, "PATH", new_path);
        set(
            &mut env,
            "CG_GCCJIT_LIBRARY_PATH",
            self.cg_dir()
                .join("jitlib/libgccjit.so")
                .display()
                .to_string(),
        );
        set(&mut env, "RUSTFLAGS", flags.trim().to_string());
        env
    }

    /// `gcc`, `as` and `ld` version lines and the CFLAGS: the `toolchain` record of a C or assembly object.
    pub fn toolchain_version(&self) -> Result<Vec<(String, String)>> {
        self.version
            .get_or_init(|| {
                let mut out = vec![];
                for n in ["gcc", "as", "ld"] {
                    let p = self.m68k(n).map_err(|e| e.0)?;
                    let o = Command::new(&p)
                        .arg("--version")
                        .output()
                        .map_err(|e| format!("{}: {e}", p.display()))?;
                    let text = String::from_utf8_lossy(&o.stdout);
                    let line = text
                        .lines()
                        .next()
                        .ok_or_else(|| format!("{}: no version", p.display()))?;
                    out.push((n.to_string(), line.to_string()));
                }
                out.push(("cflags".into(), CFLAGS.join(" ")));
                Ok(out)
            })
            .clone()
            .map_err(Error)
    }

    /// The same record as one string, which the source hash covers.
    pub fn toolchain_id(&self) -> Result<String> {
        let v = self.toolchain_version()?;
        Ok(v.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

pub struct Ran {
    pub stdout: String,
    pub stderr: String,
}

/// Run a child process; fails with its output when it exits non-zero.
pub fn run(prog: &Path, args: &[String], cwd: Option<&Path>) -> Result<Ran> {
    let mut c = Command::new(prog);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = c.output().map_err(|e| err!("{}: {e}", prog.display()))?;
    let r = Ran {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    };
    if !o.status.success() {
        return Err(err!(
            "{} failed:\n{}{}",
            prog.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
            r.stdout,
            r.stderr
        ));
    }
    Ok(r)
}

pub fn strs<S: AsRef<str>>(a: &[S]) -> Vec<String> {
    a.iter().map(|s| s.as_ref().to_string()).collect()
}

/// A directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Result<TempDir> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "{prefix}{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).map_err(|e| err!("{}: {e}", p.display()))?;
        Ok(TempDir(canon(&p)))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn read(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| err!("{}: {e}", p.display()))
}

pub fn read_text(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).map_err(|e| err!("{}: {e}", p.display()))
}
