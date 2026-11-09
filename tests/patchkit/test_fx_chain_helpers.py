"""The BASIC FX PCM peak oracle."""

import json
import struct

from tools.patchkit import pk_digikit_fx as fx


def test_pcm_peak_cli_reads_float_pcm(tmp_path, capsys):
    samples = struct.pack("<4f", 0, 0.125, -0.75, 0.5)
    body = b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 3, 2, 48000, 384000, 8, 32)
    body += b"data" + struct.pack("<I", len(samples)) + samples
    wav = tmp_path / "sine.wav"
    wav.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)
    assert fx.main(["pcm-peak", str(wav)]) == 0
    assert json.loads(capsys.readouterr().out) == {"peak": 0.75}
