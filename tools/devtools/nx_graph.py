"""The Nx project graph, as JSON."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from functools import cache
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]


@cache
def nx_graph() -> dict[str, Any]:
    """The Nx project graph, built with a private daemon-free cache; module projects come from tools/nx/modules.mjs."""
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "graph.json"
        env = {
            **os.environ,
            "NX_DAEMON": "false",
            "NX_NO_CLOUD": "true",
            "NX_WORKSPACE_DATA_DIRECTORY": str(Path(tmp) / "data"),
        }
        done = subprocess.run(
            ["mise", "exec", "--", "pnpm", "exec", "nx", "graph", f"--file={out}"],
            cwd=ROOT,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=300,
        )
        if done.returncode:
            raise RuntimeError(
                f"nx graph failed (exit {done.returncode}): {done.stderr[-2000:]}"
            )
        return json.loads(out.read_text())["graph"]
