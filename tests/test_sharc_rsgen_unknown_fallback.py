"""Execute emitted fallback code against the firmware-free native runtime."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import sharc_rsgen as rg  # noqa: E402


@pytest.mark.slow
def test_unknown_fallback_preserves_masks_budgets_and_trap_rollback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.skip("Rust toolchain unavailable")
    monkeypatch.setattr(rg, "MODEL_SAFE_ONLY", True)
    monkeypatch.setattr(rg, "CHAINING", False)
    monkeypatch.setattr(rg, "TR", None)
    insn = SimpleNamespace(type_name="2a", length_bytes=2)
    block = rg.Block(0x1000, 0x1002, [(0x1000, insn), (0x1001, insn)])
    body = rg.Body(
        block,
        None,
        0,
        [
            (0x1000, insn, "copy_first", ({0}, set(), {1}), True, None),
            (0x1001, insn, "copy_second", ({1}, set(), {2}), True, None),
        ],
        None,
        {0, 1},
        {1, 2},
    )
    emitted = rg.region_text("strict_copy_region", [body], None)
    emitted += rg.region_text("copy_region", [body], None, unknown_fallback=True)
    (tmp_path / "src").mkdir()
    native = (ROOT / "native/sharc").as_posix()
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "fallback-check"\nversion = "0.0.0"\nedition = "2024"\n'
        f'[dependencies]\nsharc-native = {{ path = "{native}" }}\n[workspace]\n'
    )
    (tmp_path / "src/main.rs").write_text(
        "pub use sharc_native::*;\nuse sharc_native::rt::*;\n"
        + emitted
        + r"""
fn copy_first(_s: &mut St, rf: &mut Rf) -> R<()> {
    rf_set(rf, 1, rf.r[0])?;
    rf.pc = 0x1001;
    Ok(())
}
fn copy_second(s: &mut St, rf: &mut Rf) -> R<()> {
    rf_set(rf, 2, rf.r[1])?;
    if s.r[4].b != 0 {
        bnd::_dm_write(s, VI::I(0x2000_0000), 4, V::c(0xAABB_CCDD), false)?;
        return Err(TRAP_INDEX);
    }
    rf.pc = 0x1002;
    Ok(())
}
fn state(value: V, limit: u64) -> Box<St> {
    let mut s = St::new(mem::Mem::new());
    s.pc_sw = 0x1000;
    s.limit = limit;
    s.r[0] = value;
    s.r[1] = V::c(9);
    s.r[2] = V::c(99);
    s.r[4] = V::c(0);
    s.mem.load(0x2000_0000, &[1, 2, 3, 4]);
    s.mem.reset();
    s
}
fn main() {
    let mut s = state(V::UNK, 2);
    assert_eq!(strict_copy_region(&mut s, 0), EXIT_BUDGET);
    assert_eq!(s.icount, 0);
    assert_eq!(s.r[1], V::c(9));
    let mut s = state(V::UNK, 2);
    let pending = Pending {
        target: Some(0x2000), call: false, slots: 2,
        return_from_call: false, return_sw: None,
    };
    s.pending = Some(pending);
    assert_eq!(copy_region(&mut s, 0), EXIT_BUDGET);
    assert_eq!(s.icount, 0);
    assert_eq!(s.pending, Some(pending));
    for value in [V::c(7), V::UNK, V { b: 5, m: 7 }] {
        let mut s = state(value, 2);
        assert_eq!(copy_region(&mut s, 0), EXIT_NEXT);
        assert_eq!(s.r[1], value);
        assert_eq!(s.r[2], value);
        assert_eq!(s.pc_sw, 0x1002);
        assert_eq!(s.icount, 2);
    }
    let mut s = state(V::UNK, 1);
    assert_eq!(copy_region(&mut s, 0), EXIT_BUDGET);
    assert_eq!(s.r[1], V::UNK);
    assert_eq!(s.r[2], V::c(99));
    assert_eq!(s.pc_sw, 0x1001);
    assert_eq!(s.icount, 1);
    let mut s = state(V::UNK, 2);
    s.r[4] = V::c(1);
    assert_eq!(copy_region(&mut s, 0), EXIT_TRAP);
    assert_eq!(s.r[1], V::UNK);
    assert_eq!(s.r[2], V::c(99));
    assert_eq!(s.pc_sw, 0x1001);
    assert_eq!(s.icount, 1);
    assert_eq!(s.mem.read_le(0x2000_0000, 4), 0x0403_0201);
}
"""
    )
    env = os.environ.copy()
    env.pop("SHARC_GEN_DIR", None)
    env["CARGO_TARGET_DIR"] = str(tmp_path / "target")
    result = subprocess.run(
        [
            cargo,
            "run",
            "--offline",
            "--quiet",
            "--manifest-path",
            str(tmp_path / "Cargo.toml"),
        ],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr
