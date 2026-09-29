//! Smoke test for the trace reader against the recorded windows under
//! `out/mmio-trace/` (firmware-derived; never committed -- CLAUDE.md). Skips
//! cleanly when the traces are not present (a fresh checkout, CI, or a
//! worktree that has not run `tools/worktree-setup.sh`), so `cargo test`
//! never depends on them.
//!
//! This only exercises the reader itself (`periph::trace`); the actual
//! oracle comparison is `mmio-replay` (`src/bin/replay.rs`), which is not a
//! `#[test]` because it is a full pass over traces up to hundreds of MB and
//! belongs to the "run tests only when semantics change" / "static check
//! before emulator run" working style (CLAUDE.md), not the default
//! `cargo test` loop.

use periph::trace::Reader;

fn trace_dir() -> std::path::PathBuf {
    // Traces live in the main tree's `out/`, linked into a worktree by
    // `tools/worktree-setup.sh`; this crate's manifest dir is
    // `<repo>/native/periph`.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/mmio-trace")
}

#[test]
fn reads_every_recorded_window_header_and_a_few_records() {
    let dir = trace_dir();
    if !dir.is_dir() {
        eprintln!(
            "skip: {} not present (see tools/worktree-setup.sh)",
            dir.display()
        );
        return;
    }
    let mut found = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mmio") {
            continue;
        }
        found += 1;
        let mut reader = Reader::open(path.to_str().unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            reader.header.get("format").and_then(|v| v.as_str()),
            Some("dt2-mmio-trace")
        );
        let mut n = 0;
        for rec in &mut reader {
            rec.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            n += 1;
            if n >= 100 {
                break;
            }
        }
        assert!(n > 0, "{}: no records read", path.display());
    }
    if found == 0 {
        eprintln!("skip: no .mmio files in {}", dir.display());
    }
}
