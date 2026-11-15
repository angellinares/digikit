"""Reference tables of the shot sound checks: the output of its float32 Python model, the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs modules/machines/shot [--check]

Run through tools/devtools/rig_refs.py. The tables are the pin once the model is retired."""

from __future__ import annotations

import re
import sys
from pathlib import Path

import numpy as np

from tools.devtools.rig_refs import write

HERE = Path(__file__).resolve().parent


def generate(check: bool) -> None:
    """The tables and the toml of the shot machine: the model's output (`shot_model.model`) for the scenarios of the retired Python test, with the rig's own
    cursor rate where the test took the model's rate from the rig. Run it again only to compare (--check); the toml is then edited by hand."""
    mod = HERE
    sys.path.insert(0, str(mod))
    import shot_model as t  # type: ignore[import-not-found]

    out = mod / "rig"
    cases: list[str] = []
    n = t.FRAMES
    pcm12 = np.concatenate([t.LEFT, t.RIGHT]).astype("<i2")
    (out / "ref").mkdir(parents=True, exist_ok=True)
    if not check:
        (out / "ref/pcm-1-2.i16").write_bytes(pcm12.tobytes())
        (out / "ref/pcm-3-4.i16").write_bytes(
            np.concatenate([t.pcm(3), t.pcm(4)]).astype("<i2").tobytes()
        )
        plane = t.sine_plane(1600, 160)
        (out / "ref/sine-plane.i16").write_bytes(
            np.concatenate([plane, plane]).astype("<i2").tobytes()
        )

    def params_text(d: dict) -> str:
        return "{ " + ", ".join(f"{k} = {v}" for k, v in d.items()) + " }"

    def pool_text(
        index=0,
        frames=n,
        rate=t.RATE_HZ,
        channels=2,
        token=t.TOKEN,
        planes="ref/pcm-1-2.i16",
        extra="",
    ) -> str:
        return (
            f"[[case.pool]]\nindex = {index}\nframes = {frames}\nrate = {rate}\nchannels = {channels}\ntoken = {token}\n"
            f'planes = "{planes}"\n{extra}\n'
        )

    def case(name, devices, pools, steps, checks, right=False) -> None:
        text = f'[[case]]\nname = "{name}"\ndevice = {devices!r}\ncontext = "friendly"\n'.replace(
            "'", '"'
        )
        text += (
            f'pool_ctl = "_shot_sample_ctl"\nright = {"true" if right else "false"}\n\n'
            + "\n".join(pools)
            + "\n"
        )
        for st in steps:
            text += "[[case.step]]\n"
            for key in ("note", "vel", "params", "blocks", "trigger", "pokes"):
                if key in st:
                    v = st[key]
                    text += f"{key} = {params_text(v) if key == 'params' else str(v).lower() if isinstance(v, bool) else v}\n"
            text += "\n"
        for c in checks:
            text += "[[case.check]]\n" + c + "\n\n"
        cases.append(text)

    def table(fname: str, arr) -> str:
        write(out / f"ref/{fname}.f32", arr, check)
        return fname

    def eq(fname: str, blocks=None, channel=None) -> str:
        r = f"blocks = [{blocks[0]}, {blocks[1]}]\n" if blocks else ""
        ch = f'channel = "{channel}"\n' if channel else ""
        return f'kind = "matches-table"\nfile = "ref/{fname}.f32"\n{r}{ch}atol = 0'

    DEF = {"sample": 0, "start": 0, "end": 0x7F00, "play": t.FWD, "pitch": 0x4000}

    def playback(
        name, devices, note=60, blocks=8, shot_kw=None, pool_kw=None, **params
    ) -> None:
        kw = shot_kw or {}
        s = t.Shot("dt2", **kw)
        gl, gr, wl, wr, (start, end, rate) = t.check(
            s, note=note, blocks=blocks, **params
        )
        assert np.array_equal(gl, wl) and np.array_equal(gr, wr)
        p = {**DEF, "sample": kw.get("index", 0) << 8, **params}
        want = t.expected_rate(
            note, p["pitch"], kw.get("rate", t.RATE_HZ), p["play"] >> 8 <= 1
        )
        tag = re.sub(r"[^a-z0-9]+", "-", name.lower().replace("+", "p")).strip("-")
        checks = [
            eq(table(f"{tag}-l", wl)),
            eq(table(f"{tag}-r", wr), channel="right"),
            f'kind = "cursor"\nblock = {blocks - 1}\nindex = 4\nequals = {want}\ntol = {max(2, abs(want) >> 17)}\nsigned = true',
        ]
        case(
            name,
            ["dt2"],
            [
                pool_text(
                    index=kw.get("index", 0),
                    channels=kw.get("channels", 2),
                    rate=kw.get("rate", t.RATE_HZ),
                )
            ],
            [{"note": note, "vel": 100, "params": p, "blocks": blocks}],
            checks,
            right=True,
        )

    # default parameters play the whole sample with distinct channels (both devices; DN2 renders L only, after the silent trig edge block)
    for device in ("dt2", "dn2"):
        s = t.Shot(device)
        gl, gr, wl, wr, _ = t.check(s)
        assert np.array_equal(gl, wl)
        first = 0 if device == "dt2" else 1
        checks = [
            eq(table(f"default-{device}-l", wl), blocks=(first, first + 8)),
            'kind = "peak"\nmin = 0.04',
            f'kind = "cursor"\nblock = {first + 7}\nindex = 4\nequals = 65536\ntol = 2\nsigned = true',
        ]
        if device == "dt2":
            checks.append(eq(table("default-dt2-r", wr), channel="right"))
        case(
            f"default parameters play the whole sample ({device})",
            [device],
            [pool_text()],
            [{"note": 60, "vel": 100, "params": DEF, "blocks": 8 + first}],
            checks,
            right=device == "dt2",
        )
    for tag, params in {
        "window": {"start": 40 * 256, "end": 100 * 256},
        "end-only": {"start": 0, "end": 50 * 256},
        "reverse": {"start": 0, "end": 90 * 256, "play": t.REV},
        "reverse-start": {"start": 20 * 256, "end": 127 * 256, "play": t.REV},
    }.items():
        playback(f"start end and play select the frames: {tag}", ["dt2"], **params)
    for tag, (note, pitch) in {
        "note+12": (72, 0x4000),
        "note-12": (48, 0x4000),
        "pitch+12": (60, 96 * 256),
        "pitch-12": (60, 32 * 256),
        "note+7,pitch+2.25": (67, 70 * 256),
    }.items():
        playback(
            f"pitch is the note plus the pitch parameter: {tag}",
            ["dt2"],
            note=note,
            pitch=pitch,
        )
    playback(
        "a source rate of 24 kHz scales the cursor",
        ["dt2"],
        blocks=12,
        shot_kw={"rate": 24000},
    )
    playback("a mono sample plays centred", ["dt2"], shot_kw={"channels": 1})
    playback("another pool entry reads its own record", ["dt2"], shot_kw={"index": 3})

    # two SMP values pick two records
    third = t.pcm(3)
    for device in ("dt2", "dn2"):
        s = t.Shot(device, index=2)
        first_w = t.model(s.left, 0, n, 65536, False, 8)
        second_w = t.model(third, 0, n, 65536, False, 8)
        e = 0 if device == "dt2" else 1
        pools = [
            pool_text(index=2),
            pool_text(
                index=5,
                token=t.TOKEN + 1,
                planes="ref/pcm-3-4.i16",
                extra="plane_l = 0x200F4000\nplane_r = 0x200FC000\n",
            ),
        ]
        a, b = (0, 8 + e), (8 + e, 16 + 2 * e)
        checks = [
            eq(table(f"two-records-{device}-first", first_w), blocks=(a[0] + e, a[1])),
            eq(
                table(f"two-records-{device}-second", second_w), blocks=(b[0] + e, b[1])
            ),
        ]
        case(
            f"two SMP values pick two records ({device})",
            [device],
            pools,
            [
                {
                    "note": 60,
                    "vel": 100,
                    "params": {**DEF, "sample": 2 << 8},
                    "blocks": 8 + e,
                },
                {
                    "note": 60,
                    "vel": 100,
                    "params": {**DEF, "sample": 5 << 8},
                    "blocks": 8 + e,
                },
            ],
            checks,
        )

    # an empty or out-of-range entry is silent
    for smp in (7, 15, 16, 127):
        case(
            f"an empty or out-of-range entry is silent: SMP {smp}",
            ["dt2"],
            [pool_text()],
            [
                {
                    "note": 60,
                    "vel": 100,
                    "params": {**DEF, "sample": smp << 8},
                    "blocks": 2,
                }
            ],
            ['kind = "peak"\nmax = 0', 'kind = "peak"\nchannel = "right"\nmax = 0'],
            right=True,
        )
    case(
        "the voice ends after the sample",
        ["dt2"],
        [pool_text()],
        [{"note": 60, "vel": 100, "params": DEF, "blocks": 12}],
        [
            '# 200 frames are 7 blocks: DK_DONE ends the voice\nkind = "voice-ends"\nafter = 0\nwithin = 10',
            'kind = "peak"\nblocks = [11, 12]\nmax = 0',
        ],
    )
    for device in ("dt2", "dn2"):
        e = 0 if device == "dt2" else 1
        checks = [
            'kind = "peak"\nblocks = [%d, %d]\nmin = 0.0001' % (e, 2 + e),
            'kind = "peak"\nblocks = [%d, %d]\nmax = 0' % (2 + e, 3 + e),
        ]
        if device == "dt2":
            checks += [
                'kind = "voice-ends"\nafter = 1\nwithin = 2',
                'kind = "peak"\nchannel = "right"\nblocks = [2, 3]\nmax = 0',
            ]
        case(
            f"a stale token is silent and ends the voice ({device})",
            [device],
            [pool_text()],
            [
                {"note": 60, "vel": 100, "params": DEF, "blocks": 2 + e},
                {"trigger": False, "blocks": 1, "pokes": [["rec", 1, t.TOKEN + 1]]},
            ],
            checks,
            right=device == "dt2",
        )
    for tag, pokes in {
        "not-ready": [["rec", 0, 0]],
        "closed": [["ctl", 0, 1]],
        "bad-fmt": [["rec", 5, 2]],
        "empty": [["rec", 2, 0]],
    }.items():
        pokes = [
            *pokes,
            ["rec", 6, 0xFFFFFFF0],
            ["rec", 7, 0xFFFFFFF0],
        ]  # a bad plane address: reading it would fault
        case(
            f"an entry that is not playable is silent with no PCM read: {tag}",
            ["dt2"],
            [pool_text()],
            [{"note": 60, "vel": 100, "params": DEF, "blocks": 2, "pokes": pokes}],
            ['kind = "peak"\nmax = 0', 'kind = "peak"\nchannel = "right"\nmax = 0'],
            right=True,
        )
    for device in ("dt2", "dn2"):
        case(
            f"instruction count is inside the ceiling ({device})",
            [device],
            [pool_text()],
            [{"note": 60, "vel": 100, "blocks": 2}],
            [
                '# worst block measured 6239 on DT2 and 6851 on DN2 (a looping 1-frame region at high pitch); the adapter\'s share and a margin\nkind = "instructions"\nof = "first"\nmax = 8699',
                'kind = "instructions"\nof = "steady"\nmax = 8499',
            ],
        )

    # live parameter changes: the model's schedule, the reader's cursor and the voice's end
    def live(
        name, first, first_blocks, later, later_blocks, expect, extra_checks=()
    ) -> None:
        s = t.Shot("dt2")
        s.start(params=first)
        ll, lr = s.step(first_blocks)
        gl, gr = s.step(later_blocks, later)
        left, right = np.concatenate([ll, gl]), np.concatenate([lr, gr])
        blocks = first_blocks + later_blocks
        reverse, start, end = expect[0]
        wl = t.model(s.left, start, end, 65536, reverse, blocks, expect)
        wr = t.model(s.right, start, end, 65536, reverse, blocks, expect)
        assert np.array_equal(left, wl) and np.array_equal(right, wr)
        tag = re.sub(r"[^a-z0-9]+", "-", name.lower().replace("+", "p")).strip("-")
        case(
            name,
            ["dt2"],
            [pool_text()],
            [
                {
                    "note": 60,
                    "vel": 100,
                    "params": {**DEF, **first},
                    "blocks": first_blocks,
                },
                {"trigger": False, "params": later, "blocks": later_blocks},
            ],
            [
                eq(table(f"{tag}-l", wl)),
                eq(table(f"{tag}-r", wr), channel="right"),
                *extra_checks,
            ],
            right=True,
        )

    live(
        "a live play flip to reverse turns at the current cursor",
        {},
        3,
        {"play": t.REV},
        8,
        {0: (False, 0, n), 3: (True, 0, n)},
        ['kind = "cursor"\nblock = 2\nindex = 2\nequals = 96'],
    )
    case(
        "a live play flip to reverse goes back from the cursor without a reset",
        ["dt2"],
        [pool_text()],
        [
            {"note": 60, "vel": 100, "params": DEF, "blocks": 3},
            {"trigger": False, "params": {"play": t.REV}, "blocks": 1},
        ],
        [
            'kind = "cursor"\nblock = 3\nindex = 2\nequals = 64',
            'kind = "cursor"\nblock = 3\nindex = 3\nequals = 0',
        ],
        right=True,
    )
    for mode, tag in ((t.FWD_L, "fwd-loop"), (t.REV_L, "rev-loop")):
        reverse = mode == t.REV_L
        s = t.Shot("dt2")
        params = {"start": t.word_of(40), "end": t.word_of(100), "play": mode}
        s.start(params=params)
        left, right = s.step(6)
        step = -65536 if reverse else 65536
        wl, wr = (
            t.model(p, 40, 100, step, reverse, 6, loop=True) for p in (s.left, s.right)
        )
        assert np.array_equal(left, wl) and np.array_equal(right, wr)
        case(
            f"a loop mode repeats the region seamlessly: {tag}",
            ["dt2"],
            [pool_text()],
            [{"note": 60, "vel": 100, "params": {**DEF, **params}, "blocks": 6}],
            [
                eq(table(f"loop-{tag}-l", wl)),
                eq(table(f"loop-{tag}-r", wr), channel="right"),
                'kind = "cursor"\nblock = 5\nindex = 5\nequals = 0',
                'kind = "voice-active"\nblock = 5',
            ],
            right=True,
        )
    # a live switch from a loop to fwd ends at the end with the ramp
    s = t.Shot("dt2")
    params = {"start": t.word_of(40), "end": t.word_of(100), "play": t.FWD_L}
    s.start(params=params)
    ll, lr = s.step(3)
    gl, gr = s.step(3, {"play": t.FWD})
    expect = {0: (False, 40, 100, True), 3: (False, 40, 100, False)}
    wl, wr = (
        t.model(p, 40, 100, 65536, False, 6, expect, loop=True)
        for p in (s.left, s.right)
    )
    assert np.array_equal(np.concatenate([ll, gl]), wl) and np.array_equal(
        np.concatenate([lr, gr]), wr
    )
    case(
        "a live switch from a loop to fwd ends at end with the ramp",
        ["dt2"],
        [pool_text()],
        [
            {"note": 60, "vel": 100, "params": {**DEF, **params}, "blocks": 3},
            {"trigger": False, "params": {"play": t.FWD}, "blocks": 3},
        ],
        [
            eq(table("loop-to-fwd-l", wl)),
            eq(table("loop-to-fwd-r", wr), channel="right"),
            'kind = "voice-ends"\nafter = 0\nwithin = 5',
        ],
        right=True,
    )
    live(
        "a live end moved behind the cursor fades the block and ends the voice",
        {},
        3,
        {"end": t.word_of(50)},
        4,
        {0: (False, 0, n), 3: (False, 0, 50)},
        ['kind = "voice-ends"\nafter = 0\nwithin = 6'],
    )
    live(
        "a live end moved above the cursor continues to the new end",
        {"end": t.word_of(100)},
        2,
        {"end": 0x7F00},
        6,
        {0: (False, 0, 100), 2: (False, 0, n)},
    )
    live(
        "a live start above the cursor does not change forward play",
        {},
        3,
        {"start": t.word_of(150)},
        5,
        {0: (False, 0, n), 3: (False, t.word_of(150), n)},
    )
    live(
        "a live start above the cursor ends reverse play with the ramp",
        {"play": t.REV},
        2,
        {"start": t.word_of(150)},
        3,
        {0: (True, 0, n), 2: (True, t.word_of(150), n)},
        [
            'kind = "cursor"\nblock = 1\nindex = 2\nequals = 135',
            'kind = "voice-ends"\nafter = 0\nwithin = 4',
        ],
    )
    live(
        "a live end at or below start in reverse ends the voice with the ramp",
        {"play": t.REV, "start": t.word_of(20)},
        2,
        {"start": t.word_of(120), "end": t.word_of(100)},
        3,
        {0: (True, 20, n), 2: (True, 120, 100)},
        ['kind = "voice-ends"\nafter = 0\nwithin = 4'],
    )
    # a pure tone keeps its level at any pitch (the fit of test_a_pure_tone_keeps_its_level_at_any_pitch)
    amp = 16000 / 32768 * float(t.STOCK_GAIN)
    for tag, note in (("unison", 60), ("octave-up", 72), ("octave-down", 48)):
        case(
            f"a pure tone keeps its level at any pitch: {tag}",
            ["dt2"],
            [pool_text(frames=1600, planes="ref/sine-plane.i16")],
            [{"note": note, "vel": 100, "params": DEF, "blocks": 20}],
            [
                f'# 300 Hz at 48 kHz scaled by the speed; the amplitude 16000 / 32768 * 0.4045 within 2 %\nkind = "tone-fit"\nfreq = {300 * 2 ** ((note - 60) / 12)!r}\n'
                f"amp = {amp!r}\ntol = {amp * 0.02!r}\nblocks = [2, 18]"
            ],
        )
    header = (
        "# the shot machine on the SHARC-only rig (replaces the retired Python test).\n"
        "# The tables ref/*.f32 are the output of the Python reader model of that test for the same scenarios (tools/devtools/rig_refs.py shot); the PCM of\n"
        "# the pool entries is ref/pcm-*.i16 (the numpy generator's int16 words). The count word of the pool's control set is checked after every block.\n\n"
    )
    if not check:
        (out / "shot.toml").write_text(header + "\n".join(cases))
        print(f"{out / 'shot.toml'}: {len(cases)} cases")
