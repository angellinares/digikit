"""A block body goes on after a MODE1 write that changes no bit the engine acts on."""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.sharc_dev import sharc_rsgen as rg  # noqa: E402


def test_mode1_continue_decides_by_engine_bits() -> None:
    assert rg.mode1_continue(0x0, 0x8000, 0x0) == (True, None)
    assert rg.mode1_continue(0x0, 0x8, 0x0) == (False, None)
    assert rg.mode1_continue(0x0, 0x1000, 0x0) == (False, None)
    assert rg.mode1_continue(0x0, 0x800, 0x0) == (False, None)
    assert rg.mode1_continue(None, 0x8000, 0x0) == (False, None)
    assert rg.mode1_continue(0x8000, None, 0x0) == (True, 0x0)
    assert rg.mode1_continue(0x8000, None, None) == (False, None)
    assert rg.mode1_continue(0x8, None, 0x0) == (False, None)


@pytest.mark.slow
def test_body_goes_on_after_mode1_write_with_exact_bank_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.skip("Rust toolchain unavailable")
    monkeypatch.setattr(rg, "MODEL_SAFE_ONLY", True)
    monkeypatch.setattr(rg, "CHAINING", False)
    monkeypatch.setattr(rg, "TR", None)
    insn = SimpleNamespace(type_name="2a", length_bytes=2)

    def body(cont: dict) -> rg.Body:
        block = rg.Block(
            0x1000,
            0x1003,
            [(0x1000, insn), (0x1001, insn), (0x1002, insn)],
        )
        return rg.Body(
            block,
            0x0,
            0,
            [
                (0x1000, insn, "write_mode1", ({5}, set(), {114}), True, 0x0),
                (0x1001, insn, "copy_second", ({0}, set(), {1}), True, 0x0),
                (0x1002, insn, "copy_third", ({0}, set(), {2}), True, 0x0),
            ],
            None,
            {0, 5},
            {114, 1, 2},
            cont,
        )

    emitted = rg.region_text("static_region", [body({0: None})], None)
    emitted += rg.region_text("guard_region", [body({0: 0x0})], None)
    (tmp_path / "src").mkdir()
    native = (ROOT / "packages/emulator/sharc").as_posix()
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "mode1-check"\nversion = "0.0.0"\nedition = "2024"\n'
        f'[dependencies]\nsharc-native = {{ path = "{native}" }}\n[workspace]\n'
    )
    (tmp_path / "src/main.rs").write_text(
        "pub use sharc_native::*;\nuse sharc_native::rt::*;\n"
        + emitted
        + r"""
fn write_mode1(s: &mut St, rf: &mut Rf) -> R<()> {
    bnd::_bank_request(s, rf.r[5])?;
    rf.r[114] = rf.r[5];
    rf.pc = 0x1001;
    Ok(())
}
fn copy_second(s: &mut St, rf: &mut Rf) -> R<()> {
    rf_set(rf, 1, rf.r[0])?;
    if s.r[4].b != 0 {
        return Err(TRAP_INDEX);
    }
    rf.pc = 0x1002;
    Ok(())
}
fn copy_third(_s: &mut St, rf: &mut Rf) -> R<()> {
    rf_set(rf, 2, rf.r[0])?;
    rf.pc = 0x1003;
    Ok(())
}
fn state(mode1_write: Int) -> Box<St> {
    let mut s = St::new(mem::Mem::new());
    s.cfg.bank_model = true;
    s.bank_active_mask = 0;
    s.bank_pending_mask = -1;
    s.bank_requested_mask = -1;
    s.pc_sw = 0x1000;
    s.limit = 1000;
    s.r[0] = V::c(7);
    s.r[1] = V::c(9);
    s.r[2] = V::c(99);
    s.r[4] = V::c(0);
    s.r[5] = V::c(mode1_write);
    s.r[114] = V::c(0);
    s.mem.reset();
    s
}
fn main() {
    // static: all three run, one completion per instruction
    let mut s = state(0x8000);
    assert_eq!(static_region(&mut s, 0), EXIT_NEXT);
    assert_eq!(s.icount, 3);
    assert_eq!(s.pc_sw, 0x1003);
    assert_eq!(s.bank_requested_mask, -1);
    assert_eq!(s.bank_pending_mask, -1);
    assert_eq!(s.bank_active_mask, 0);
    // guard fails: the request is left for the engine
    let mut s = state(0x8000);
    assert_eq!(guard_region(&mut s, 0), EXIT_NEXT);
    assert_eq!(s.icount, 1);
    assert_eq!(s.pc_sw, 0x1001);
    assert_eq!(s.bank_requested_mask, 0);
    assert_eq!(s.bank_pending_mask, -1);
    // guard holds
    let mut s = state(0x0);
    assert_eq!(guard_region(&mut s, 0), EXIT_NEXT);
    assert_eq!(s.icount, 3);
    assert_eq!(s.pc_sw, 0x1003);
    assert_eq!(s.bank_requested_mask, -1);
    assert_eq!(s.bank_pending_mask, -1);
    assert_eq!(s.bank_active_mask, 0);
    // step 1 traps after step 0's completion
    let mut s = state(0x8000);
    s.r[4] = V::c(1);
    assert_eq!(static_region(&mut s, 0), EXIT_TRAP);
    assert_eq!(s.icount, 1);
    assert_eq!(s.pc_sw, 0x1001);
    assert_eq!(s.bank_pending_mask, 0);
    assert_eq!(s.bank_requested_mask, -1);
    assert_eq!(s.r[1], V::c(9));
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
