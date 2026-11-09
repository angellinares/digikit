"""core/params port rules: the Python scan that records the sites agrees with the recorded ports, and its pieces."""

import os
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = str(REPO_ROOT)

from tools.patchkit import port_resolve as pr  # noqa: E402


def test_the_site_scan_finds_references_and_clamp_bounds_but_not_loop_ends():
    # base 0x1000: bound, a reference into [0x2000, 0x2000 + 5 * 60), a loop end (bne), a bound (bhi)
    main = bytes.fromhex(
        "0c800000000555c141f9000020040c820000000566000c8100000004620400000000"
    )
    refs, bounds = pr.scan_param_sites(main, 0x1000, 0x2000, 5)
    assert refs == [0x1000 + 10]
    assert bounds == [(0x1000, 5), (0x1000 + 22, 4)]
    digest = pr.params_sites_digest(refs, bounds)
    assert len(digest) == 64 and digest != pr.params_sites_digest(refs, bounds[:1])


def test_rules_are_declared_for_the_three_anchors():
    declarations = {
        entry["name"]: entry
        for entries in tomllib.loads(
            (REPO_ROOT / "devices/fw/param.toml").read_text()
        ).values()
        for entry in entries
    }
    params_rules = [
        r
        for r in pr.fw_rules(REPO_ROOT)
        if r["name"].startswith("cf.param.")
        and r["_source"] == "fw/param"
        and "core/params" in declarations[r["name"]].get("users", [])
    ]
    for device in ("dt2", "dn2"):
        rules = {r["name"]: r for r in params_rules if device in r["devices"]}
        assert set(rules) == {
            "cf.param.data.descriptor_table",
            "cf.param.sites.descriptor_table.ref",
            "cf.param.sites.descriptor_table.bound",
            "cf.param.fn.value_class",
        }
        assert rules["cf.param.data.descriptor_table"]["kind"] == "cf_params_t1"
        assert rules["cf.param.sites.descriptor_table.ref"]["kind"] == "cf_params_refs"
        assert (
            rules["cf.param.sites.descriptor_table.bound"]["kind"] == "cf_params_bounds"
        )
        assert (
            len(rules["cf.param.fn.value_class"]["hash"]) == 32
            and rules["cf.param.fn.value_class"]["len"] == 10
        )
    # The table scans are DT2/DN2-specific: every rule must name its devices, so a new device never matches by accident.
    assert all(
        r.get("devices") and set(r["devices"]) <= {"dt2", "dn2"} for r in params_rules
    )


@pytest.mark.parametrize("version", ["1.17"])
def test_the_scan_reproduces_the_recorded_port(version):
    syx = os.path.join(ROOT, f"Digitakt_II_OS{version}.syx")
    port = os.path.join(ROOT, "devices", "ports", f"dt2-{version}.toml")
    if not (os.path.exists(syx) and os.path.exists(port)):
        pytest.skip("needs the stock image and its port")
    with open(port, "rb") as f:
        anchors = {a["name"]: a for a in tomllib.load(f)["anchor"]}
    main = pr.sections_of(type(pr.ROOT)(syx))[3]
    t1 = anchors["cf.param.data.descriptor_table"]
    refs, bounds = pr.scan_param_sites(main, 0x40000400, t1["addr"], t1["len"] // 60)
    ref_names = [
        n for n in anchors if n.startswith("cf.param.sites.descriptor_table.ref.")
    ]
    bound_names = [
        n for n in anchors if n.startswith("cf.param.sites.descriptor_table.bound.")
    ]
    # the accessor of the second table (cf.param.fn.value_class) holds one bound the module replaces
    assert len(refs) == len(ref_names) == 56
    assert len(bounds) - 1 == len(bound_names) == 54
    assert sorted(anchors[n]["addr"] for n in ref_names) == refs


READER = bytes.fromhex(
    "202f00040c800000000555c141f9000020007301c0812200e589ed889081203008044e75"
)


def _synthetic_main():
    """The reader of the row count (a bound; its `lea` holds a reference to the table), a word pointing into the
    table, and a second bound after it. Loaded at 0x1000; the table is at 0x2000 with 5 rows."""
    ref = (0x2004).to_bytes(4, "big")
    other = bytes.fromhex("0c8200000004620400000000")
    return READER + ref + other


def test_multi_site_rules_give_one_anchor_per_site_and_honour_exclusions():
    from types import SimpleNamespace

    main = _synthetic_main()
    n = len(READER)
    cf = SimpleNamespace(
        main=main,
        base=0x1000,
        bytes_at=lambda a, k: main[a - 0x1000 : a - 0x1000 + k],
    )
    refs = {"name": "r", "kind": "cf_params_refs"}
    got = pr.resolve_cf_multi(cf, refs, {})
    # the `lea` operand in the reader and the standalone word
    assert [(a["addr"], a["len"], a["kind"]) for a in got] == [
        (0x1000 + 14, 4, "data"),
        (0x1000 + n, 4, "data"),
    ]
    assert got[1]["sha256"] == pr.sha(main[n : n + 4])
    bounds = {"name": "b", "kind": "cf_params_bounds"}
    got = pr.resolve_cf_multi(cf, bounds, {})
    # the compare in the reader (count) and the one after it (count - 1, next byte 0x62)
    assert [a["addr"] for a in got] == [0x1000 + 4, 0x1000 + n + 4]
    assert all(a["len"] == 7 and a["kind"] == "code" for a in got)
    assert got[0]["sha256"] == pr.sha(main[4:11])
    # exclude_anchors drops the sites inside an anchor resolved earlier; an unknown name is an error
    ex = {**bounds, "exclude_anchors": ["fn"]}
    fn = {"fn": {"addr": 0x1000 + n + 4, "len": 12}}
    assert [a["addr"] for a in pr.resolve_cf_multi(cf, ex, fn)] == [0x1000 + 4]
    with pytest.raises(pr.ResolveError):
        pr.resolve_cf_multi(cf, ex, {})
