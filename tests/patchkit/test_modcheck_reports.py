"""`pk_modcheck.measured_costs`: a cost comes only from a report that this worktree wrote."""

import json

import pytest

from tools.patchkit import pk_modcheck


def _report(path, worktree, steady=2449):
    body = {"cases": [{"steady": steady}, {"steady": 10}]}
    if worktree is not None:
        body["worktree"] = worktree
    path.write_text(json.dumps(body))


def test_reads_the_cost_of_a_report_from_this_worktree(tmp_path, monkeypatch):
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_machine_module", lambda _m: True)
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_fx_module", lambda _m: False)
    _report(tmp_path / "a__b@dt2.json", str(pk_modcheck.ROOT.resolve()))
    costs = pk_modcheck.measured_costs("a/b", ["dt2"], 0.0, tmp_path)
    assert costs == {"dt2": {pk_modcheck.COST_VOICE: 2449}}


@pytest.mark.parametrize("owner", ["/other/lane", None])
def test_refuses_a_report_of_another_worktree_or_of_none(tmp_path, monkeypatch, owner):
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_machine_module", lambda _m: True)
    monkeypatch.setattr(pk_modcheck.pk_conform, "is_fx_module", lambda _m: False)
    _report(tmp_path / "a__b@dt2.json", owner, steady=2658)
    with pytest.raises(RuntimeError, match="not by this one"):
        pk_modcheck.measured_costs("a/b", ["dt2"], 0.0, tmp_path)
