"""The TOML text of a `patchkit synth-dkm` spec, for the Python tests that need a small packed module (the TypeScript tests use tests/regression/runtime/synth.ts)."""


def spec(name: str, *, dsp: dict | None = None) -> str:
    """DSP is `{image, bss, voices: [{type}]}`."""
    lines = [f'name = "{name}"']
    if dsp is not None:
        lines += ["[dsp]", f"image = {dsp.get('image', 64)}"]
        if dsp.get("bss"):
            lines.append(f"bss = {dsp['bss']}")
        for v in dsp.get("voices", []):
            lines += ["[[dsp.voice]]", f"type = {v['type']}"]
    return "\n".join(lines) + "\n"
