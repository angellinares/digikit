"""`module:new -- KIND AUTHOR/NAME` (tools/patchkit/pk_new.py): the module templates of sdk/templates."""

import tomllib

import pytest

from tools.patchkit import pk_new

EXPECTED_TYPE = {"machine": "machine", "fx": "fx", "tool": "service"}


@pytest.mark.parametrize("kind", sorted(pk_new.KIND_DIRS))
def test_a_template_is_copied_with_every_token_replaced(kind, tmp_path):
    base = tmp_path / "drone"
    values = pk_new.tokens("alice/drone")
    written = pk_new.write_template(kind, base, values)
    assert written
    for path in written:
        assert not pk_new.TOKEN_RE.search(str(path.relative_to(base)))
        assert not pk_new.TOKEN_RE.search(path.read_text()), path
    assert any("drone" in p.name for p in written if p.suffix in {".s", ".c"})
    man = tomllib.loads((base / "module.toml").read_text())["module"]
    assert man["id"] == "alice/drone" and man["author"] == "alice"
    assert man["type"] == EXPECTED_TYPE[kind]


def test_an_existing_directory_is_refused(tmp_path):
    (tmp_path / "drone").mkdir()
    with pytest.raises(SystemExit, match="exists"):
        pk_new.create("machine", "alice/drone", tmp_path)


@pytest.mark.parametrize(
    "id_",
    [
        "drone",
        "alice/Drone",
        "alice/my-drone",
        "alice/1drone",
        "core/drone",
        "framework/drone",
    ],
)
def test_an_invalid_id_is_refused(id_, tmp_path):
    with pytest.raises(SystemExit):
        pk_new.create("fx", id_, tmp_path)
    assert not any(tmp_path.iterdir())


def test_the_fx_tab_and_the_title_stay_within_the_schema_limits():
    values = pk_new.tokens("alice/drone", short="ABCDEF")
    assert values["TAB"] == "ABC"
    with pytest.raises(SystemExit, match="title"):
        pk_new.tokens("alice/a_name_that_is_longer_than_23_chars")
