"""tools/devtools/layercheck.py on small temporary trees: one tree that holds, and one failure per rule."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tools.devtools import layercheck

MANIFEST = """[module]
id = "{id}"
version = "0.1.0"
name = "{id}"
category = "{category}"
{role}type = "framework"
author = "digikit"
status = "experimental"
summary = "t"
"""


def write(root: Path, rel: str, text: str) -> Path:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding="utf-8")
    return p


def module(
    root: Path, rel: str, mid: str, category: str, role: str | None, extra: str = ""
) -> None:
    r = f'role = "{role}"\n' if role else ""
    write(
        root,
        f"{rel}/module.toml",
        MANIFEST.format(id=mid, category=category, role=r) + extra,
    )


def dko(root: Path, rel: str, undefined: list[str]) -> None:
    syms = [{"name": n, "section": None, "global": True, "value": 0} for n in undefined]
    syms.append({"name": "main_fn", "section": ".text", "global": True, "value": 0})
    write(root, rel, json.dumps({"arch": "cf", "symbols": syms}))


@pytest.fixture
def tree(tmp_path: Path) -> Path:
    """A tree that holds: runtime, installer, one adapter, one app, one content module."""
    r = tmp_path
    write(r, "sdk/sdk.toml", '[[import]]\nname = "fw_alloc"\nsig = "u8 *(u32 size)"\n')
    write(r, "sdk/include/dk/api.h", "/* sdk */\n")
    write(r, "sdk/asm/dk_machine.inc", "/* sdk asm */\n")
    write(r, "sdk/templates/m/src/dsp/m.s", ".EXTERN _dv_exp2;\n")
    module(r, "modules/core/runtime", "core/runtime", "core", "runtime")
    write(r, "modules/core/runtime/include/dkrt.h", "/* runtime */\n")
    module(r, "modules/core/expansion", "core/expansion", "core", "installer")
    module(r, "modules/core/adapters/ui", "core/ui", "core", "adapter")
    write(r, "modules/core/adapters/ui/include/dk_ui_core.h", "/* ui */\n")
    write(
        r,
        "modules/core/adapters/ui/src/ui.c",
        '#include "dk_fw_mem.h"\n#include "dk_ui_core.h"\n',
    )
    write(r, "modules/core/fw/include/dk_fw_mem.h", "/* fw */\n")
    write(r, "modules/core/fw/inc/dk_fw_menu.inc", "/* fw */\n")
    module(r, "modules/core/apps/menu", "core/menu", "core", "app")
    write(
        r,
        "modules/core/apps/menu/src/menu.c",
        '#include <stdint.h>\n#include <dk/api.h>\n#include "dkrt.h"\n#include <dk_ui_core.h>\n#include "own.h"\n',
    )
    write(r, "modules/core/apps/menu/src/own.h", "/* own */\n")
    module(
        r,
        "modules/core/apps/menu",
        "core/menu",
        "core",
        "app",
        '\n[[object]]\npath = "build/menu.dko.json"\nplace = "cf.expansion"\n',
    )
    dko(
        r,
        "modules/core/apps/menu/build/menu.dko.json",
        ["dk_ui_text", "dkrt_scan", "fw_alloc"],
    )
    module(r, "modules/fx/gain", "digikit/gain", "fx", None)
    write(
        r,
        "modules/fx/gain/src/dsp/gain.s",
        '#include "dk_machine.inc"\n.EXTERN _dv_exp2;\n',
    )
    write(r, "modules/fx/gain/src/cf/gain.c", "#include <dk/api.h>\n")
    return r


def rules(root: Path) -> list[str]:
    return [v.rule for v in layercheck.check(root)]


def test_a_valid_tree_holds(tree: Path) -> None:
    assert layercheck.check(tree) == []


def test_role_is_required_and_must_fit_the_folder(tree: Path) -> None:
    module(tree, "modules/core/adapters/ui", "core/ui", "core", None)
    (v,) = layercheck.check(tree)
    assert v.rule == "roles" and "no `role`" in v.message
    assert v.where.startswith("modules/core/adapters/ui/module.toml:")
    module(tree, "modules/core/adapters/ui", "core/ui", "core", "app")
    (v,) = [v for v in layercheck.check(tree) if v.rule == "roles"]
    assert (
        v.rule == "roles" and 'role = "app"' in v.message and '"adapter"' in v.message
    )


def test_a_core_module_outside_the_role_folders_is_refused(tree: Path) -> None:
    module(tree, "modules/core/stray", "core/stray", "core", "adapter")
    (v,) = layercheck.check(tree)
    assert v.rule == "roles" and "no role folder" in v.message


def test_app_may_not_include_the_firmware_bindings(tree: Path) -> None:
    write(
        tree,
        "modules/core/apps/menu/src/menu.c",
        '#include "dkrt.h"\n#include "dk_fw_mem.h"\n',
    )
    (v,) = layercheck.check(tree)
    assert v.rule == "sources" and v.where == "modules/core/apps/menu/src/menu.c:2"
    assert "dk_fw_mem.h" in v.message and "Instead:" in str(v)


def test_assembly_include_of_a_firmware_file_is_refused(tree: Path) -> None:
    write(tree, "modules/core/apps/menu/src/shim.s", '.include "dk_fw_menu.inc"\n')
    (v,) = layercheck.check(tree)
    assert v.rule == "sources" and v.where.endswith("shim.s:1")


def test_content_module_may_not_include_runtime_or_adapter_headers(tree: Path) -> None:
    write(
        tree,
        "modules/fx/gain/src/cf/gain.c",
        '#include <dk/api.h>\n#include "dkrt.h"\n#include "dk_ui_core.h"\n',
    )
    found = layercheck.check(tree)
    assert [v.rule for v in found] == ["sources", "sources"]
    assert [v.where.rsplit(":", 1)[1] for v in found] == ["2", "3"]
    assert "dk_ui_core.h" in found[1].message


def test_content_module_may_not_reach_into_another_module(tree: Path) -> None:
    write(tree, "modules/fx/other/src/o.h", "/* other */\n")
    module(tree, "modules/fx/other", "digikit/other", "fx", None)
    write(tree, "modules/fx/gain/src/cf/gain.c", '#include "../../../other/src/o.h"\n')
    (v,) = layercheck.check(tree)
    assert v.rule == "sources" and "digikit/other" in v.message


def test_rust_module_may_not_use_the_firmware_crate(tree: Path) -> None:
    write(
        tree,
        "modules/fx/gain/src/rs/Cargo.toml",
        '[package]\nname = "g"\n[dependencies]\ndkfw = { path = "../../../../core/fw/rs" }\n',
    )
    write(tree, "modules/fx/gain/src/rs/src/lib.rs", "use dkfw::mem;\n")
    found = layercheck.check(tree)
    assert {v.rule for v in found} == {"sources"}
    assert {v.where.rsplit(":", 1)[0].rsplit("/", 1)[1] for v in found} == {
        "Cargo.toml",
        "lib.rs",
    }


def test_rust_module_may_depend_on_the_sdk_crate(tree: Path) -> None:
    write(
        tree,
        "modules/fx/gain/src/rs/Cargo.toml",
        '[package]\nname = "g"\n[dependencies]\ndkrt-sdk = { path = "../../../../core/runtime/sdk" }\n',
    )
    write(tree, "modules/core/runtime/sdk/Cargo.toml", '[package]\nname = "dkrt-sdk"\n')
    assert layercheck.check(tree) == []


def test_object_may_not_need_a_firmware_anchor(tree: Path) -> None:
    dko(
        tree,
        "modules/core/apps/menu/build/menu.dko.json",
        ["dk_ui_text", "cf.menu.fn.add_item"],
    )
    (v,) = layercheck.check(tree)
    assert v.rule == "objects" and "cf.menu.fn.add_item" in v.message


def test_object_may_not_need_an_unknown_symbol(tree: Path) -> None:
    dko(tree, "modules/core/apps/menu/build/menu.dko.json", ["_fw_private"])
    (v,) = layercheck.check(tree)
    assert v.rule == "objects" and "_fw_private" in v.message


def test_externs_may_not_map_a_symbol_to_an_anchor(tree: Path) -> None:
    module(
        tree,
        "modules/core/apps/menu",
        "core/menu",
        "core",
        "app",
        '\n[[object]]\npath = "build/menu.dko.json"\nplace = "cf.expansion"\n'
        '\n[object.externs]\n_tail = "hook.cf.menu.site.tail"\n',
    )
    found = layercheck.check(tree)
    assert [v.rule for v in found] == ["objects"]
    assert "hook.cf.menu.site.tail" in found[0].message


def test_app_has_no_ops_and_no_claims(tree: Path) -> None:
    module(
        tree,
        "modules/core/apps/menu",
        "core/menu",
        "core",
        "app",
        '\n[[object]]\npath = "build/menu.dko.json"\nplace = "cf.expansion"\n'
        '\n[[op]]\nop = "hook"\nhook = "cf.menu.hook.x"\nto = "dk_hook"\n'
        '\n[[claim]]\nresource = "cf.menu.site.x"\n',
    )
    found = [v for v in layercheck.check(tree) if v.rule == "apps"]
    assert len(found) == 2
    assert {v.message.split("[[")[1].split("]]")[0] for v in found} == {"op", "claim"}
    assert all(v.where.startswith("modules/core/apps/menu/module.toml:") for v in found)


def test_listed_firmware_patch_modules_keep_their_anchors(tree: Path) -> None:
    module(
        tree,
        "tests/fixtures/modules/registry-demo",
        "test/registry-demo",
        "test",
        None,
        '\n[[object]]\npath = "build/p.dko.json"\nplace = "dsp.l2"\n'
        '\n[object.externs]\n_q = "dsp.master.fn.q31"\n',
    )
    dko(tree, "out/cache/dko/test/registry-demo/build/p.dko.json", ["_q"])
    assert layercheck.check(tree) == []


def test_a_content_module_may_not_commit_an_object(tree: Path) -> None:
    dko(tree, "modules/fx/patch/build/p.dko.json", [])
    module(
        tree,
        "modules/fx/patch",
        "digikit/patch",
        "fx",
        None,
        '\n[[object]]\npath = "build/p.dko.json"\nplace = "cf.expansion"\n',
    )
    (v,) = layercheck.check(tree)
    assert v.rule == "objects" and "object file in its tree" in v.message
    assert v.where == "modules/fx/patch/build/p.dko.json:1"
    # the cache is the place for it; the symbols are still checked there
    (tree / "modules/fx/patch/build/p.dko.json").unlink()
    dko(tree, "out/cache/dko/digikit/patch/build/p.dko.json", ["_fw_private"])
    (v,) = layercheck.check(tree)
    assert v.rule == "objects" and "_fw_private" in v.message


def test_ui_functions_must_be_imports_the_runtime_links(tree: Path) -> None:
    ui = "typedef void dk_ui_act_fn(void *ctx, int arg);\nint dk_ui_open(dk_ui_menu *m);\n"
    write(tree, "sdk/include/dk/ui.h", ui)
    imp = '[[import]]\nname = "dk_ui_open"\nsig = "i32 (u8 *menu)"\nrequires = "core/ui"\n'
    write(tree, "sdk/sdk.toml", (tree / "sdk/sdk.toml").read_text() + imp)
    runtime = (
        '[[object]]\npath = "build/r.dko.json"\nplace = "cf.expansion"\n'
        'optional = ["dk_ui_open"]\n'
    )
    module(tree, "modules/core/runtime", "core/runtime", "core", "runtime", runtime)
    # a typedef is no function; the import, the header and the optional list agree
    assert layercheck.check(tree) == []
    # a function that is not an import
    write(tree, "sdk/include/dk/ui.h", ui + "void dk_ui_close(dk_ui_menu *m);\n")
    (v,) = layercheck.check(tree)
    assert v.rule == "imports" and "dk_ui_close" in v.message
    assert v.where == "sdk/include/dk/ui.h:3"
    # an import that the header does not declare
    write(tree, "sdk/include/dk/ui.h", "typedef int dk_ui_t;\n")
    (v,) = layercheck.check(tree)
    assert v.rule == "imports" and "dk_ui_open" in v.message
    # an import the runtime does not link
    write(tree, "sdk/include/dk/ui.h", ui)
    module(tree, "modules/core/runtime", "core/runtime", "core", "runtime")
    (v,) = layercheck.check(tree)
    assert v.rule == "imports" and "optional list" in v.message


def sc(root: Path) -> list[layercheck.Violation]:
    return [v for v in layercheck.check(root) if v.rule == "self-contained"]


def test_self_contained_names_of_a_module_outside_it_are_hits(tree: Path) -> None:
    write(tree, "modules/fx/gain/tests/own.py", 'ID = "digikit/gain"\n')  # inside: fine
    write(tree, "tests/other.py", "ok = 1\nuse('digikit/gain')\n")
    write(tree, "tools/path.sh", "build modules/fx/gain\n")
    write(tree, "tools/near.sh", "digikit/gain-like fx/gainer\n")  # other names
    got = {v.where: v for v in sc(tree)}
    assert set(got) == {"tests/other.py:2", "tools/path.sh:1"}
    v = got["tests/other.py:2"]
    assert "digikit/gain named outside modules/fx/gain" in v.message
    assert "tests/" in v.instead and "placeholder" in v.instead


def test_self_contained_docs_agents_and_fixtures_may_name_modules(tree: Path) -> None:
    for f in (
        "docs/findings/x.txt",
        "notes/readme.md",
        ".agents/skill/a.txt",
        "tests/fixtures/modules/f/src.c",
    ):
        write(tree, f, "digikit/gain\n")
    assert sc(tree) == []


def test_self_contained_a_host_module_is_exempt(tree: Path) -> None:
    write(tree, "apps/page.ts", "'digikit/gain'\n")
    assert len(sc(tree)) == 1
    module(tree, "modules/fx/gain", "digikit/gain", "fx", None, "host = true\n")
    # `host` must sit in [module]: the extra text lands there because the manifest has no later table
    assert sc(tree) == []
