"""Generate tests/data/edge_goldens.json from the Python app's own code.

Edge cases the fixture libraries do not contain (mono, silence, too short,
the block-size boundary, >5 channels, odd sample rates, gating, formatting
ties, non-finite album inputs). Signals are integer PCM from a 31-bit LCG so
the Rust test regenerates them bit-for-bit; they are scaled by 1/32768 as
soundfile scales 16-bit FLAC, then measured with pyloudnorm exactly as
musaic/services/audio_analysis.py::_measure_track_loudness does, and the
album/format cases call that module's _aggregate_album / _format_gain /
_format_peak directly.

Run with the Python app (tag python-final) importable as `musaic`, from a
checkout root (no network, no ~/.musaic access):

    MUSAIC_CONFIG_DIR="$(mktemp -d)" python -I \
        rust/crates/musaic-dsp/tests/data/gen_edge_goldens.py
"""
from __future__ import annotations

import json
import math
import os
import sys
from pathlib import Path

assert os.environ.get("MUSAIC_CONFIG_DIR"), "set MUSAIC_CONFIG_DIR to a scratch dir"

import numpy as np  # noqa: E402
import pyloudnorm  # noqa: E402

from musaic.services import audio_analysis  # noqa: E402

OUT = Path(__file__).with_name("edge_goldens.json")


def lcg_pcm(seed: int, channels: int, segments: list[dict]) -> np.ndarray:
    state = seed
    out = []
    for seg in segments:
        amp = seg["amp"]
        for _ in range(seg["frames"] * channels):
            state = (state * 1103515245 + 12345) & 0x7FFFFFFF
            out.append(((state >> 8) % (2 * amp + 1)) - amp if amp else 0)
    return np.array(out, dtype=np.int64)


def num(x: float):
    """JSON has no inf/nan; encode them as strings."""
    if math.isfinite(x):
        return x
    return "nan" if math.isnan(x) else ("inf" if x > 0 else "-inf")


SIGNALS = [
    {"name": "mono-noise-44100-2s", "rate": 44100, "channels": 1, "seed": 1,
     "segments": [{"frames": 88200, "amp": 8000}]},
    {"name": "stereo-noise-48000-gated", "rate": 48000, "channels": 2, "seed": 2,
     "segments": [{"frames": 48000, "amp": 20000}, {"frames": 96000, "amp": 30},
                  {"frames": 24000, "amp": 3000}]},
    {"name": "stereo-silence-44100-1s", "rate": 44100, "channels": 2, "seed": 3,
     "segments": [{"frames": 44100, "amp": 0}]},
    {"name": "stereo-near-silent-44100-1s", "rate": 44100, "channels": 2, "seed": 4,
     "segments": [{"frames": 44100, "amp": 1}]},
    {"name": "mono-too-short-44100", "rate": 44100, "channels": 1, "seed": 5,
     "segments": [{"frames": 17639, "amp": 5000}]},
    {"name": "mono-exactly-one-block-44100", "rate": 44100, "channels": 1, "seed": 6,
     "segments": [{"frames": 17640, "amp": 5000}]},
    {"name": "five-channels-22050-odd-length", "rate": 22050, "channels": 5, "seed": 7,
     "segments": [{"frames": 27211, "amp": 12000}]},
    {"name": "six-channels-44100", "rate": 44100, "channels": 6, "seed": 8,
     "segments": [{"frames": 44100, "amp": 1000}]},
    {"name": "stereo-full-scale-96000", "rate": 96000, "channels": 2, "seed": 9,
     "segments": [{"frames": 100001, "amp": 32767}]},
]

ALBUMS = [
    [[60.0, -10.0, 0.9], [60.0, -30.0, 0.5]],
    [[60.0, -20.0, 0.8], [60.0, "-inf", 0.0]],
    [[0.0, -20.0, 0.3], [12.5, -14.25, 0.7], [7.75, -22.1, 0.7]],
    [[60.0, "-inf", 0.4], [30.0, "nan", 0.6]],
    [[0.0, -12.0, 0.2]],
    [],
    [[159.05333333333334, -18.22853944850848, 0.9393310546875],
     [212.0, -16.5, 0.996063232421875], [1.0, -60.0, 0.1]],
]

GAINS = [-18.0, -18.125, -18.375, -17.995, -18.004, -17.999, -18.001, -22.0, -3.0,
         -40.123456, "-inf", "nan"]
PEAKS = [0.0, 1.0, 0.9393310546875, 0.3653564453125, 0.0000005, 0.0000015, 0.9999995,
         0.123456789]


def parse(x):
    return float(x) if isinstance(x, str) else x


def main() -> None:
    signals = []
    for sig in SIGNALS:
        pcm = lcg_pcm(sig["seed"], sig["channels"], sig["segments"])
        data = pcm.astype(np.float64) / 32768.0
        if sig["channels"] > 1:
            data = data.reshape(-1, sig["channels"])
        rec = dict(sig)
        rec["check"] = {"sum": int(pcm.sum()), "sum_sq": int((pcm * pcm).sum()),
                        "len": int(pcm.size)}
        try:
            meter = pyloudnorm.Meter(sig["rate"])
            lufs = float(meter.integrated_loudness(data))
            peak = float(np.max(np.abs(data))) if data.size else 0.0
            rec["result"] = {"lufs": num(lufs), "peak": peak,
                             "track_gain": audio_analysis._format_gain(-18.0, lufs),
                             "track_peak": audio_analysis._format_peak(peak)}
        except Exception as e:  # recorded exactly as the Python app would log it
            rec["result"] = {"error": f"{type(e).__name__}: {e}"}
        signals.append(rec)

    albums = []
    for tracks in ALBUMS:
        agg = audio_analysis._aggregate_album([tuple(parse(v) for v in t) for t in tracks])
        albums.append({
            "tracks": tracks,
            "result": None if agg is None else {
                "lufs": agg[0], "peak": agg[1],
                "album_gain": audio_analysis._format_gain(-18.0, agg[0]),
                "album_peak": audio_analysis._format_peak(agg[1]),
            },
        })

    out = {
        "generator": "rust/crates/musaic-dsp/tests/data/gen_edge_goldens.py",
        "environment": {"python": sys.version.split()[0], "numpy": np.__version__,
                        "pyloudnorm": getattr(pyloudnorm, "__version__", "0.2.0")},
        "reference_loudness": f"{audio_analysis.REPLAYGAIN_REFERENCE_LUFS:.2f} LUFS",
        "signals": signals,
        "albums": albums,
        "format_gain": [{"measured": g, "formatted": audio_analysis._format_gain(-18.0, parse(g))}
                        for g in GAINS],
        "format_peak": [{"peak": p, "formatted": audio_analysis._format_peak(p)} for p in PEAKS],
    }
    OUT.write_text(json.dumps(out, indent=1) + "\n")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
