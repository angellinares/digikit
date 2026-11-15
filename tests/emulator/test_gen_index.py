"""The generated core is picked by the SHA-256 of the packed DSP image."""

from __future__ import annotations

from support.paths import REPO_ROOT

ROOT = str(REPO_ROOT)

from tools.emulator import digi_env  # noqa: E402


def test_registered_core_is_found_by_image_content(monkeypatch, tmp_path):
    monkeypatch.setattr(digi_env, "GEN_INDEX", tmp_path / "gen-index.json")
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    image = tmp_path / "image.bin"
    image.write_bytes(b"dt2 image")
    other = tmp_path / "other.bin"
    other.write_bytes(b"dn2 image")
    gen = tmp_path / "gen"
    gen.mkdir()
    assert digi_env.gen_for_image(image) is None
    digi_env.register_gen(image, gen)
    assert digi_env.gen_for_image(image) == gen.resolve()
    # A copy of the image elsewhere resolves the same; another image does not.
    copy = tmp_path / "copy.bin"
    copy.write_bytes(image.read_bytes())
    assert digi_env.generated_dir(image=copy) == gen.resolve()
    assert digi_env.generated_dir(image=other) == digi_env.DEFAULT_GENERATED


def test_environment_overrides_the_registry(monkeypatch, tmp_path):
    monkeypatch.setattr(digi_env, "GEN_INDEX", tmp_path / "gen-index.json")
    image = tmp_path / "image.bin"
    image.write_bytes(b"x")
    gen = tmp_path / "gen"
    gen.mkdir()
    digi_env.register_gen(image, gen)
    monkeypatch.setenv("SHARC_GEN_DIR", str(tmp_path / "forced"))
    assert digi_env.generated_dir(image=image) == tmp_path / "forced"


def test_device_variable_wins_over_the_shared_one_and_the_registry(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(digi_env, "GEN_INDEX", tmp_path / "gen-index.json")
    image = tmp_path / "image.bin"
    image.write_bytes(b"x")
    gen = tmp_path / "gen"
    gen.mkdir()
    digi_env.register_gen(image, gen)
    for name in ("SHARC_GEN_DIR", "SHARC_GEN_DIR_DN2", "SHARC_GEN_DIR_DT2"):
        monkeypatch.delenv(name, raising=False)
    # none set: the index
    assert digi_env.generated_dir(image=image, device="dt2") == gen.resolve()
    # only SHARC_GEN_DIR set: both devices use it
    monkeypatch.setenv("SHARC_GEN_DIR", str(tmp_path / "shared"))
    assert digi_env.generated_dir(image=image, device="dn2") == tmp_path / "shared"
    assert digi_env.generated_dir(image=image, device="dt2") == tmp_path / "shared"
    # DN2 set: DN2 uses it, DT2 falls back to SHARC_GEN_DIR
    monkeypatch.setenv("SHARC_GEN_DIR_DN2", str(tmp_path / "dn2"))
    assert digi_env.generated_dir(image=image, device="dn2") == tmp_path / "dn2"
    assert digi_env.generated_dir(image=image, device="dt2") == tmp_path / "shared"
    # DT2 set: DT2 uses it; without SHARC_GEN_DIR the other device uses the index
    monkeypatch.delenv("SHARC_GEN_DIR")
    monkeypatch.delenv("SHARC_GEN_DIR_DN2")
    monkeypatch.setenv("SHARC_GEN_DIR_DT2", str(tmp_path / "dt2"))
    assert digi_env.generated_dir(image=image, device="dt2") == tmp_path / "dt2"
    assert digi_env.generated_dir(image=image, device="dn2") == gen.resolve()
    # no device given: only SHARC_GEN_DIR counts
    assert digi_env.generated_dir(image=image) == gen.resolve()
