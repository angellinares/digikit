//! Trace marks (`DK_TRACE_LOG` in C, `DK_STEP` in SHARC+ sources): a build without `--trace` is the same file as the sources without the
//! marks; a build with `--trace` carries the trace bit and imports `_dk_step`. Skips when the m68k-elf toolchain is not installed.

use patchkit::build::toolchain::Env;
use patchkit::dkm::Dkm;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A repository root of its own whose only fixture module is `test/trace-demo` without its trace marks: the lines of the macros are
/// dropped, `DK_TRACE_INIT(api)` becomes `(void)api` (the parameter is used either way). The module keeps its id (the id feeds the
/// registry records), so it cannot sit next to the real fixture. `modules`, `devices`, `sdk` and the toolchain are the repository's.
fn stripped_root(dir: &Path) {
    use std::os::unix::fs::symlink;
    let real = root();
    for n in ["modules", "devices", "sdk"] {
        symlink(real.join(n), dir.join(n)).unwrap();
    }
    std::fs::create_dir_all(dir.join("out")).unwrap();
    symlink(Env::new(&real).tools, dir.join("out/tools")).unwrap();
    let from = real.join("tests/fixtures/modules/trace-demo");
    let to = dir.join("tests/fixtures/modules/trace-demo");
    for rel in ["module.toml", "src/module.c", "src/dsp/trace_demo.s"] {
        let text = std::fs::read_to_string(from.join(rel)).unwrap();
        let text = if rel == "module.toml" {
            text
        } else {
            text.lines()
                .filter(|l| {
                    !(l.contains("DK_STEP(")
                        || l.contains("DK_TRACE_LOG(")
                        || l.contains("dk_trace.inc")
                        || l.contains("<dk/trace.h>"))
                })
                .map(|l| l.replace("DK_TRACE_INIT(api);", "(void)api;"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        std::fs::create_dir_all(to.join(rel).parent().unwrap()).unwrap();
        std::fs::write(to.join(rel), text).unwrap();
    }
}

/// `patchkit --root ROOT pack test/trace-demo --device dt2 [--trace]`: the file's bytes.
fn pack(root: &Path, out: &Path, trace: bool) -> Vec<u8> {
    let mut c = Command::new(env!("CARGO_BIN_EXE_patchkit"));
    c.arg("--root")
        .arg(root)
        .args(["pack", "test/trace-demo", "--device", "dt2", "-o"])
        .arg(out);
    if trace {
        c.arg("--trace");
    }
    let r = c.output().unwrap();
    assert!(
        r.status.success(),
        "pack failed:\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    std::fs::read(out).unwrap()
}

#[test]
fn trace_marks_cost_nothing_when_off_and_show_when_on() {
    if Env::new(&root()).m68k("gcc").is_err() {
        eprintln!(
            "SKIP: the m68k-elf toolchain is not installed (uv run python -m tools.patchkit.pk_toolchains m68k)"
        );
        return;
    }
    let d = std::env::temp_dir().join(format!("pk-trace-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    stripped_root(&d);
    let out = d.join("out.dkm");
    let here = root();

    let off = pack(&here, &out, false);
    let plain = pack(&d, &out, false);
    assert!(
        off == plain,
        "without --trace the file must equal the one of the sources without the marks"
    );
    assert!(!Dkm::parse(&off).unwrap().trace);

    let on = pack(&here, &out, true);
    let dkm = Dkm::parse(&on).unwrap();
    assert!(dkm.trace, "a traced file carries the trace bit");
    assert!(on.len() > off.len());
    assert_eq!(
        Dkm::parse(&off).unwrap().runtime_min,
        dkm.runtime_min,
        "the bit is not part of the runtime version"
    );
    let dsp = patchkit::dkmdsp::DspSection::parse(&dkm.dsp).unwrap();
    assert!(
        dsp.imports.iter().any(|i| i == "_dk_step"),
        "imports: {:?}",
        dsp.imports
    );
    // the traced objects did not replace the plain ones in the cache
    assert_eq!(off, pack(&here, &out, false));

    let _ = std::fs::remove_dir_all(&d);
}
