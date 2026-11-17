"""The run-time registry (core/dynreg, the runtime's registry.rs, `patchkit pack` of a dynamic module): machines and
parameters that a `.dkm` adds at run time (docs/design/digikit-registry.md, finding 30).

The committed objects are current; the packer lowers a dynamic module's typed sections into `register` records
deterministically; the emulator proofs are the boot-time scenarios of tests/digikit (the firmware's own list getter and rows with the module
selected at boot, the PCM of the machine). The hot-load proofs that were here (digikit-hot-reload-v1:tools/patchkit/pk_registry.py: mailbox load,
refuse, unload, the panel frames) are archived with hot reload (tag digikit-hot-reload-v1)."""

import json

from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import pk_dkrt, pk_toolchains  # noqa: E402

DYNREG = ROOT / "modules/core/adapters/dynreg"
RUNTIME = ROOT / "modules/core/runtime"


def test_committed_objects_are_current_and_plain_coldfire():
    for dev in ("dt2", "dn2"):
        obj = json.loads((DYNREG / f"build/dynreg-{dev}.dko.json").read_text())
        assert {r["type"] for r in obj["relocs"]} <= {"cf.32", "cf.pc32", "cf.pc16"}
    front_end.build_module(DYNREG, check=True)
    if pk_toolchains.codegen_gcc_ready():
        front_end.build_module(RUNTIME, check=True)


def test_pack_refuses_a_module_for_the_wrong_device(tmp_path):
    tools = pk_dkrt.Tools()
    # a Digitakt module (dyn-dsp-hang, ports dt2-1.17) is refused for the Digitone
    for module, device, ports in (("test/dyn-dsp-hang", "dn2", "dt2-1.17"),):
        r = tools.run(
            tools.patchkit,
            "--root",
            ROOT,
            "pack",
            module,
            "--device",
            device,
            "-o",
            tmp_path / "z.dkm",
        )
        assert r.returncode != 0
        assert (
            f"is not built for {device}: its ports are {ports}" in r.stdout + r.stderr
        )


def test_pack_refuses_a_patch_time_object(tmp_path):
    # registry-demo is a patch-time module (it defines no dk_module): it cannot be packed as a .dkm for either device
    tools = pk_dkrt.Tools()
    for device in ("dt2", "dn2"):
        r = tools.run(
            tools.patchkit,
            "--root",
            ROOT,
            "pack",
            "test/registry-demo",
            "--device",
            device,
            "-o",
            tmp_path / "y.dkm",
        )
        assert r.returncode != 0
        assert "defines no dk_module" in r.stdout + r.stderr


def test_pack_of_a_builder_module_is_deterministic(tmp_path):
    # the Digitone II module: machine (own_dsp), parameters and one page, declared by builder calls in init; the keys table and the DSP section come from
    # module.toml and the objects only, so two packs give the same bytes
    tools = pk_dkrt.Tools()
    files = []
    for name in ("a.dkm", "b.dkm"):
        out = tmp_path / name
        tools.patchkit_run("pack", "test/dyn-dn2", "--device", "dn2", "-o", out)
        files.append(out.read_bytes())
    assert files[0] == files[1]
