#!/usr/bin/env python3
"""Canonically compare two generated SHARC discovery reports.

This tool consumes reports emitted by ``tools/sharc_discover.py``; it never
opens firmware.  Its output is structural static evidence only.  In
particular, matching normalized function signatures does not establish a
shared algorithm, runtime path, or semantic role.

Usage:
    uv run python tools/sharc_compare.py FIRST.json SECOND.json [-o OUTPUT.json]
"""
from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path
from typing import Any, Mapping

SCHEMA = "sharc-discovery-comparison/v1"
DISCOVERY_SCHEMA = "sharc-discovery/v1"
STRUCTURAL_FEATURES = (
    "calls",
    "compute_total",
    "float_alu",
    "float_mul",
    "indirect_calls",
    "mac",
    "mem_load",
    "mem_store",
)


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _unavailable(reason: str) -> dict[str, str]:
    return {"status": "unavailable", "reason": reason}


def _available(value: Any) -> dict[str, Any]:
    return {"status": "available", "value": value}


def _coverage_count(report: Mapping[str, Any], coverage_key: str, list_key: str) -> dict[str, Any]:
    coverage = report.get("coverage")
    if isinstance(coverage, Mapping) and isinstance(coverage.get(coverage_key), int):
        return _available(coverage[coverage_key])
    values = report.get(list_key)
    if isinstance(values, list):
        return _available(len(values))
    return _unavailable(f"report has neither coverage.{coverage_key} nor {list_key}")


def _identity(report: Mapping[str, Any]) -> dict[str, Any]:
    provenance = report.get("provenance")
    blob = provenance.get("blob") if isinstance(provenance, Mapping) else None
    manifest = provenance.get("manifest") if isinstance(provenance, Mapping) else None
    result: dict[str, Any] = {"report_schema": report.get("schema")}
    if isinstance(blob, Mapping):
        result["image_sha256"] = blob.get("sha256")
        result["image_size"] = blob.get("size")
    else:
        result["image_sha256"] = None
        result["image_size"] = None
    result["manifest_schema"] = manifest.get("schema") if isinstance(manifest, Mapping) else None
    result["manifest_sha256"] = manifest.get("sha256") if isinstance(manifest, Mapping) else None
    return result


def _form_histogram(report: Mapping[str, Any]) -> dict[str, Any]:
    sites = report.get("indirect_sites")
    if not isinstance(sites, list):
        return _unavailable("report does not contain indirect_sites; no instruction-form histogram is emitted by discovery reports")
    forms = collections.Counter(
        site["form"] for site in sites
        if isinstance(site, Mapping) and isinstance(site.get("form"), str)
    )
    return _available({name: forms[name] for name in sorted(forms)})


def _stop_histogram(report: Mapping[str, Any]) -> dict[str, Any]:
    coverage = report.get("coverage")
    stops = coverage.get("stop_reasons") if isinstance(coverage, Mapping) else None
    if not isinstance(stops, Mapping) or not all(isinstance(key, str) and isinstance(value, int) for key, value in stops.items()):
        return _unavailable("report does not contain coverage.stop_reasons")
    return _available({key: stops[key] for key in sorted(stops)})


def _function_signatures(report: Mapping[str, Any]) -> dict[str, Any]:
    functions = report.get("functions")
    if not isinstance(functions, list):
        return _unavailable("report does not contain functions")
    signatures: collections.Counter[str] = collections.Counter()
    references: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for function in functions:
        if not isinstance(function, Mapping):
            return _unavailable("functions contains a non-object entry")
        n_insns, features = function.get("n_insns"), function.get("features")
        callers, callees = function.get("callers"), function.get("callees")
        unresolved = function.get("unresolved_callees")
        if not isinstance(n_insns, int) or not isinstance(features, Mapping) or not isinstance(callers, list) or not isinstance(callees, list) or not isinstance(unresolved, list):
            return _unavailable("functions lacks n_insns, features, callers, callees, or unresolved_callees needed for normalized structural signatures")
        if any(not isinstance(features.get(key), int) for key in STRUCTURAL_FEATURES):
            return _unavailable("functions lacks the stable numeric decoder feature subset")
        signature = {
            "callee_count": len(callees),
            "caller_count": len(callers),
            "features": {key: features[key] for key in STRUCTURAL_FEATURES},
            "n_insns": n_insns,
            "unresolved_callee_count": len(unresolved),
        }
        encoded = _canonical(signature)
        signatures[encoded] += 1
        references[encoded].append({
            "entry_sw": function.get("entry_sw"),
            "id": function.get("id"),
            "label": function.get("label"),
        })
    return _available({
        "counts": signatures,
        "references": {
            signature: sorted(items, key=_canonical)
            for signature, items in sorted(references.items())
        },
    })


def _signature_rows(counts: Mapping[str, int]) -> list[dict[str, Any]]:
    rows = []
    for signature in sorted(counts):
        try:
            decoded = json.loads(signature)
        except json.JSONDecodeError as error:  # pragma: no cover - produced by _canonical
            raise AssertionError("internal function signature is not canonical JSON") from error
        rows.append({"signature": decoded, "count": counts[signature]})
    return rows


def _structural_signatures(left: Mapping[str, Any], right: Mapping[str, Any]) -> dict[str, Any]:
    first, second = _function_signatures(left), _function_signatures(right)
    if first["status"] != "available" or second["status"] != "available":
        reasons = [item["reason"] for item in (first, second) if item["status"] != "available"]
        return _unavailable("; ".join(reasons))
    first_counts, second_counts = first["value"]["counts"], second["value"]["counts"]
    first_refs, second_refs = first["value"]["references"], second["value"]["references"]
    common = {key: min(first_counts[key], second_counts[key]) for key in first_counts.keys() & second_counts.keys()}
    first_only = {key: first_counts[key] - common.get(key, 0) for key in first_counts if first_counts[key] > common.get(key, 0)}
    second_only = {key: second_counts[key] - common.get(key, 0) for key in second_counts if second_counts[key] > common.get(key, 0)}
    one_to_one = []
    for signature in common:
        if first_counts[signature] == second_counts[signature] == 1:
            decoded = _signature_rows({signature: 1})[0]["signature"]
            references = [first_refs[signature][0], second_refs[signature][0]]
            one_to_one.append({
                "functions": references,
                "same_entry_sw": references[0]["entry_sw"] == references[1]["entry_sw"],
                "signature": decoded,
            })
    one_to_one.sort(key=lambda row: (-row["signature"]["n_insns"], _canonical(row)))
    return _available({
        "definition": "n_insns, stable numeric decoder feature subset, and direct/resolved/unresolved call counts; excludes addresses, table literals, identifiers, labels, and runtime claims",
        "common": _signature_rows(common),
        "first_only": _signature_rows(first_only),
        "one_to_one_candidates": one_to_one,
        "second_only": _signature_rows(second_only),
    })


def _summary(report: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "identity": _identity(report),
        "counts": {
            "functions": _coverage_count(report, "functions", "functions"),
            "instructions": _coverage_count(report, "decoded_instruction_pcs", "decoded_instruction_pcs"),
            "indirect_sites": _coverage_count(report, "indirect_sites", "indirect_sites"),
            "pointer_table_runs": _coverage_count(report, "pointer_table_runs", "pointer_tables"),
        },
        "indirect_site_form_histogram": _form_histogram(report),
        "stop_reason_histogram": _stop_histogram(report),
    }


def compare_reports(first: Mapping[str, Any], second: Mapping[str, Any]) -> dict[str, Any]:
    """Return a path- and input-order-independent structural comparison."""
    ordered = sorted((first, second), key=_canonical)
    return {
        "schema": SCHEMA,
        "scope": "static structural evidence only; unavailable fields are not inferred",
        "reports": [_summary(report) for report in ordered],
        "normalized_function_signatures": _structural_signatures(*ordered),
    }


def _load_report(path: Path) -> dict[str, Any]:
    try:
        report = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read report: {error}") from error
    if not isinstance(report, dict):
        raise ValueError("report must be a JSON object")
    if report.get("schema") != DISCOVERY_SCHEMA:
        raise ValueError(f"report schema must be {DISCOVERY_SCHEMA}")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args(argv)
    try:
        result = compare_reports(_load_report(args.first), _load_report(args.second))
    except ValueError as error:
        parser.error(str(error))
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
    else:
        print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
