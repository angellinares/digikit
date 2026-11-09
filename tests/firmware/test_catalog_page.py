"""Row selection of tools/firmware/catalog_page.py, on synthetic inputs."""

import csv
import json
import subprocess
import unittest
from pathlib import Path
from urllib.parse import unquote

import pytest

from tools.firmware import catalog_page


def row(addr: int, score: float, anchors: str = "") -> dict[str, str]:
    return {"addr": hex(addr), "score": str(score), "anchors": anchors}


class PickRowsTest(unittest.TestCase):
    def test_labelled_first_then_score_skipping_anchored(self) -> None:
        members = {
            1: [
                row(0x10, 1.0),
                row(0x20, 5.0),
                row(0x30, 9.0, anchors="cf.x"),
                row(0x40, 3.0),
            ],
            2: [row(0x50, 2.0)],
        }
        picked = catalog_page.pick_rows(members, {0x10}, 3)
        self.assertEqual([r["addr"] for r in picked], ["0x10", "0x20", "0x40", "0x50"])

    def test_labelled_rows_are_kept_over_the_cap(self) -> None:
        members = {1: [row(0x10, 1.0), row(0x20, 2.0), row(0x30, 3.0)]}
        picked = catalog_page.pick_rows(members, {0x10, 0x20}, 1)
        self.assertEqual([r["addr"] for r in picked], ["0x20", "0x10"])


class SelectGroupsTest(unittest.TestCase):
    def test_small_groups_kept_only_when_labelled(self) -> None:
        groups = [
            {"id": "1", "size": "10"},
            {"id": "2", "size": "3"},
            {"id": "3", "size": "3"},
        ]
        keep, small = catalog_page.select_groups(groups, {3}, 10)
        self.assertEqual([g["id"] for g in keep], ["1", "3"])
        self.assertEqual(small, {3})


def inventory():
    rows = []
    for addr, group in ((0x10, 1), (0x20, 2), (0x30, -1)):
        rows.append(
            {
                **row(addr, 5.0, anchors="cf.existing" if addr == 0x30 else ""),
                "group": str(group),
                "name": 'Bitmap::<tag> & "quoted" </script>',
                "work_name": "",
                "class": "View",
                "anchor_grade": "1",
                "tags": "ui_helper,near:cf.param",
                "ui_callers": "2",
                "ui_strings": "3",
                "hint": "A\tB\nC",
                "pair_addr": "0x40",
                "pair_method": "strict",
                "pair_same_hash": "1",
                "rule_both": "0",
                "h_strict": "abcdef",
                "hash_count": "1",
                "stable": "",
                "rule_kind": "cf_func",
                "rule_ordinal": "",
                "caller_classes": "View:2",
                "callees_known": "draw",
                "size": "24",
            }
        )
    groups = [
        {
            "id": str(gid),
            "size": str(size),
            "grade": "1",
            "anchored": "0",
            "top_classes": "View",
            "top_strings": "UI",
            "top_tags": "ui_helper",
        }
        for gid, size in ((1, 10), (2, 1))
    ]
    glabels = {1: [{"label": "Views", "sdk_relevance": "UI", "summary": "Drawing"}]}
    labels = {
        0x10: [
            {
                "role": "Draw",
                "sdk_use": "yes",
                "evidence": "A\tB\nC",
                "proposed_anchor": "cf.draw",
            }
        ]
    }
    return rows, groups, glabels, labels


def payload(full=True):
    rows, groups, glabels, labels = inventory()
    return catalog_page.build_payload(
        "dt2-1.17",
        "dn2-1.12",
        rows,
        groups,
        [["0x10", "0x30", 2]],
        glabels,
        labels,
        10,
        1,
        full,
    )


def test_full_inventory_keeps_small_anchored_and_ungrouped_rows():
    full = payload()
    assert {r["a"] for r in full["rows"]} == {"0x10", "0x20", "0x30"}
    assert {g["id"] for g in full["groups"]} == {1, 2, -1}
    assert full["total_groups"] == 3
    assert full["rows"][0]["nb"] == [["0x30", inventory()[0][2]["name"], True, 2]]
    assert [r["a"] for r in payload(False)["rows"]] == ["0x10"]


def test_review_tsv_roundtrips_evidence_labels_and_pair_links(tmp_path):
    rows, _, glabels, labels = inventory()
    path = tmp_path / "review.tsv"
    catalog_page.write_review_tsv(
        str(path), "dt2-1.17", "dn2-1.12", rows, glabels, labels
    )
    exported = catalog_page.read_tsv(str(path))
    assert len(exported) == 3
    assert exported[0]["hint"] == exported[0]["label_evidence"] == "A\tB\nC"
    assert exported[0]["label_sdk_use"] == "yes"
    assert exported[0]["group_label"] == "Views"
    assert exported[0]["review_link"] == "dt2-1.17-review.html#r-0x10"
    assert exported[0]["pair_review_link"] == "dn2-1.12-review.html#r-0x40"
    for key, value in rows[0].items():
        assert exported[0][key] == value


def test_custom_output_tsv_links_to_actual_html(tmp_path, monkeypatch):
    rows, groups, _, _ = inventory()
    atlas = tmp_path / "atlas"
    atlas.mkdir()
    monkeypatch.setattr(catalog_page.fw, "ATLAS_DIR", str(atlas))
    for suffix, records in (("-catalog.tsv", rows), ("-catalog-groups.tsv", groups)):
        with (atlas / ("dt2-1.17" + suffix)).open("w", newline="") as f:
            writer = csv.DictWriter(f, list(records[0]), delimiter="\t")
            writer.writeheader()
            writer.writerows(records)
    (atlas / "dt2-1.17-catalog-graph.json").write_text('{"edges": []}')
    output = tmp_path / "custom page.html"
    assert catalog_page.main(["dt2-1.17", "--out", str(output)]) == 0
    exported = catalog_page.read_tsv(str(output.with_suffix(".tsv")))
    for record in exported:
        filename, address = record["review_link"].split("#r-")
        assert filename == "custom%20page.html"
        assert output.parent / unquote(filename) == output
        assert (output.parent / unquote(filename)).is_file()
        assert address == record["addr"]


def test_pair_outside_inventory_retains_evidence_without_dead_link(tmp_path):
    rows, groups, glabels, labels = inventory()
    data = catalog_page.build_payload(
        "dt2-1.17", "dn2-1.12", rows, groups, [], glabels, labels, 10, 20, True, {0x50}
    )
    assert data["rows"][0]["pair"] == "0x40"
    assert data["rows"][0]["pm"] == "strict"
    assert data["rows"][0]["pair_url"] == ""
    path = tmp_path / "review.tsv"
    catalog_page.write_review_tsv(
        str(path), "dt2-1.17", "dn2-1.12", rows, glabels, labels, {0x50}
    )
    assert catalog_page.read_tsv(str(path))[0]["pair_review_link"] == ""


def test_render_escapes_html_without_changing_payload():
    data = payload()
    page = catalog_page.render(
        "<div>__SUBTITLE__</div><script>const DATA = __DATA__;</script>",
        data,
        '<img src=x onerror="x">',
    )
    assert "<img src=x" not in page
    assert page.count("</script>") == 1
    embedded = page.split("const DATA = ", 1)[1].removesuffix(";</script>")
    assert json.loads(embedded) == data
    assert (
        catalog_page.review_link('../x"<', '0x10"')
        == "..%2Fx%22%3C-review.html#r-0x10%22"
    )


@pytest.mark.parametrize("full", [False, True])
def test_page_deep_links_local_triage_and_export(tmp_path, full):
    template = Path(catalog_page.TEMPLATE).read_text()
    page = catalog_page.render(template, payload(full), "Review")
    script = page.split("<script>", 1)[1].split("</script>", 1)[0]
    path = tmp_path / "page.js"
    path.write_text(script)
    # Run the generated script against a small DOM double: only the active group
    # should render, a hidden deep link must reveal its row, and local decisions
    # must survive a new page instance and be exportable/importable.
    harness = r"""
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(process.argv[1], 'utf8');
class Element {
  constructor(tag) { this.tag = tag; this.nodeType = 1; this.children = []; this.events = {}; this.value = ''; this.classList = {add(){}, remove(){}}; }
  setAttribute(k,v) { this[k] = v; }
  append(...kids) { this.children.push(...kids); }
  replaceChildren(...kids) { this.children = kids; }
  addEventListener(k,v) { this.events[k] = v; }
  scrollIntoView() {}
  click() {}
}
const storage = new Map();
storage.set('digikit-catalog-decisions-dt2-1.17', JSON.stringify({
  'dt2-1.17-0x20': {image:'dt2-1.17',addr:'0x20',status:'later',anchor:'cf.hidden',note:'outside current selection'},
}));
let exported;
function boot(claude) {
  const ids = ['q','f-lab','f-open','f-grade','f-rel','groups','detail','progress','foot','banner','export','import','storage-mode','find-bitmaps','find-params'];
  const roots = Object.fromEntries(ids.map(id => [id,new Element('root')]));
  roots['f-lab'].checked = true;
  roots.import.disabled = true;
  function find(node,id) { return node.id === id ? node : node.children.map(c => find(c,id)).find(Boolean); }
  const context = vm.createContext({
    document: {createElement: t => new Element(t), createTextNode: t => ({nodeType:3,text:t,children:[]}), getElementById: id => roots[id] || Object.values(roots).map(r=>find(r,id)).find(Boolean)},
    window: {claude,addEventListener(){}}, location: {hash:'#r-0x30'}, history: {replaceState(){}},
    localStorage: {getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)},
    matchMedia:()=>({matches:true}), setTimeout:()=>{}, Blob,
    URL: {createObjectURL:b=>{exported=b;return 'blob:test';},revokeObjectURL(){}},
  });
  vm.runInContext(source,context);
  return {context,roots};
}
(async()=> {
  let {context,roots} = boot();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(vm.runInContext('state.group',context),-1);
  assert.equal(roots.detail.children[1].children.length,1);
  assert.equal(vm.runInContext('state.canWrite && state.local',context),true);
  const article = roots.detail.children[1].children[0];
  function all(node) { return [node,...node.children.flatMap(all)]; }
  assert(all(article).some(n=>n.href==='dn2-1.12-review.html#r-0x40'));
  assert(all(article).some(n=>n.href==='#r-0x30'));
  vm.runInContext("$('f-rel').value='UI'; jump('0x30')",context);
  assert.equal(vm.runInContext('state.group',context),-1);
  assert.equal(roots['f-rel'].value,'');
  await vm.runInContext("save(rowByAddr.get('0x30'),'keep','cf.test','local note',{})",context);
  assert(storage.size===1);
  ({context,roots}=boot());
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(vm.runInContext("statusOf('0x30')",context),'keep');
  roots.export.events.click();
  const records = (await exported.text()).trim().split('\n').map(line=>JSON.parse(line));
  assert.equal(records.find(d=>d.addr==='0x20').note,'outside current selection');
  const decision = records.find(d=>d.addr==='0x30');
  assert.equal(decision.note,'local note');
  decision.status='later';
  await roots.import.events.change({target:{files:[{text:async()=>JSON.stringify(decision)}],value:''}});
  assert.equal(vm.runInContext("statusOf('0x30')",context),'later');
  let written;
  const db = {
    collection:()=>({limit:()=>({onSnapshot:fn=>fn({docs:[]})})}),
    doc:()=>({set:async body=>{written=body;},delete:async()=>{}}),
  };
  ({context,roots}=boot({use:async kind=>kind==='db'?db:{can:async()=>true}}));
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(vm.runInContext('state.local',context),false);
  assert.equal(roots.import.disabled,true);
  await vm.runInContext("save(rowByAddr.get('0x10'),'keep','cf.db','database note',{})",context);
  assert.equal(written.note,'database note');
  assert.equal(written.image,'dt2-1.17');
  ({context,roots}=boot({use:async kind=>kind==='db'?db:{can:async()=>false}}));
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(vm.runInContext('state.canWrite',context),false);
})().catch(e=>{console.error(e);process.exitCode=1;});
"""
    subprocess.run(
        ["node", "-e", harness, str(path)], check=True, capture_output=True, text=True
    )


if __name__ == "__main__":
    unittest.main()
