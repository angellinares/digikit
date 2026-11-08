"""Resolver for the emulator tier settings in config/emulator.json.

Precedence, lowest to highest: built-in defaults, the file, then environment variables.
An explicit variable wins, including an empty one. `resolve` is the only place that applies
these rules; the launchers, the PGO tool, the daemon client and the test tools call it.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import os
import pathlib
from collections.abc import Mapping
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
CONFIG_PATH = ROOT / "config" / "emulator.json"

# Environment variable and table key of each region list.
LISTS = (
    ("SHARC_FAST_REGIONS", "fast_regions"),
    ("SHARC_BLOCKTRANS", "blocktrans"),
    ("SHARC_FAST_FALLBACK_REGIONS", "fallback_regions"),
)
FALLBACK = "SHARC_FAST_FALLBACK_REGIONS"
ENV_FALLBACK_OFF = ("SHARC_FAST_REGIONS", "SHARC_FAST_AUTO")


def load(path: pathlib.Path | str = CONFIG_PATH) -> dict[str, Any]:
    return json.loads(pathlib.Path(path).read_text())


def firmware_table(config: Mapping[str, Any]) -> dict[str, dict[str, str]]:
    """The per-firmware entries (keyed by SHA-256 of the stock .syx)."""
    return {k: v for k, v in config.items() if not k.startswith("_")}


def digest_of(syx: pathlib.Path | str) -> str:
    """SHA-256 of a firmware file, "" when it cannot be read."""
    try:
        return hashlib.sha256(pathlib.Path(syx).read_bytes()).hexdigest()
    except OSError:
        return ""


@dataclasses.dataclass
class Resolution:
    """What `resolve` found.

    add: env additions (names carry the slot suffix; nothing the environment already sets).
    explicit: the region lists the environment sets itself (slot-suffixed names, empty values kept).
    off: DIGI_EMU_FAST=0, so no region list is added.
    sources: name -> "env", "file" or "context", for the startup line.
    """

    add: dict[str, str]
    explicit: dict[str, str]
    off: bool
    sources: dict[str, str]
    context: str | None
    auto: str | None = None

    def effective(self, name: str, slot: str = "") -> str:
        """The value a run uses for a region list: env if set, else the added value, else ""."""
        name += slot
        return self.explicit[name] if name in self.explicit else self.add.get(name, "")


def startup_line(
    context: str, env: Mapping[str, str], *resolutions: Resolution | None
) -> str:
    """One line for the log: the context, SHARC_FAST_AUTO, list sizes in ENV and which values the environment set."""

    def count(name: str) -> int:
        return len([x for x in env.get(name, "").split(",") if x.strip()])

    added: set[str] = set()
    overrides: set[str] = set()
    for res in resolutions:
        if res is not None:
            added |= {n for n in res.add if res.sources.get(n) == "file"}
            overrides |= {n for n, s in res.sources.items() if s == "env"}
    overrides |= {name for name, _ in LISTS if name in env and name not in added}
    auto = env.get("SHARC_FAST_AUTO")
    return (
        f"tiers: context={context} "
        f"SHARC_FAST_AUTO={'-' if auto is None else repr(auto)} "
        f"regions={count('SHARC_FAST_REGIONS')} fallback={count(FALLBACK)} "
        f"blocktrans={count('SHARC_BLOCKTRANS')} "
        f"from-env={','.join(sorted(overrides)) or '-'}"
    )


def resolve(
    context: str | None,
    digest: str,
    slot: str = "",
    environ: Mapping[str, str] | None = None,
    *,
    config: Mapping[str, Any] | None = None,
    lists: bool = True,
    fallback: bool = True,
    generic_core: bool = False,
) -> Resolution:
    """Env additions for one core.

    CONTEXT names the caller in `_contexts` (None: no context defaults). DIGEST is the SHA-256
    of the firmware ("" when unknown). SLOT is "" for the first core and "_2" for the second;
    it suffixes the region list names. ENVIRON defaults to os.environ.

    Rules: an explicit variable wins, including an empty one. DIGI_EMU_FAST=0 adds no region
    list. An explicit SHARC_FAST_REGIONS{slot} or SHARC_FAST_AUTO suppresses the fallback list.
    Context defaults are added when the variable is unset, also under DIGI_EMU_FAST=0, and do not
    suppress the fallback list. An entry with `when: generic-core` needs GENERIC_CORE.
    Options: lists=False adds only context defaults; fallback=False never adds the fallback list.
    """
    environ = os.environ if environ is None else environ
    config = load() if config is None else config
    entry = firmware_table(config).get(digest, {})
    off = environ.get("DIGI_EMU_FAST") == "0"
    add: dict[str, str] = {}
    explicit: dict[str, str] = {}
    sources: dict[str, str] = {}
    if lists:
        for name, key in LISTS:
            full = name + slot
            if name == FALLBACK and not fallback:
                continue
            if full in environ:
                explicit[full] = environ[full]
                sources[full] = "env"
                continue
            if off:
                continue
            if name == FALLBACK and (
                (ENV_FALLBACK_OFF[0] + slot) in environ
                or ENV_FALLBACK_OFF[1] in environ
            ):
                continue
            if value := entry.get(key, ""):
                add[full] = value
                sources[full] = "file"
    defaults = dict(config.get("_contexts", {}).get(context, {})) if context else {}
    condition = defaults.pop("when", None)
    if condition is None or (condition == "generic-core" and generic_core):
        for name, value in defaults.items():
            if name in environ:
                sources[name] = "env"
            else:
                add[name] = value
                sources[name] = "context"
    if "SHARC_FAST_AUTO" in environ:
        sources["SHARC_FAST_AUTO"] = "env"
    auto = environ.get("SHARC_FAST_AUTO", add.get("SHARC_FAST_AUTO"))
    return Resolution(add, explicit, off, sources, context, auto)
