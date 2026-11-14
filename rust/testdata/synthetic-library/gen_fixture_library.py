"""Generate the synthetic FLAC test library from albums.py (next to this file).

    python3 testdata/synthetic-library/gen_fixture_library.py [--out DIR] [--force]   # from rust/

Needs Python 3.10+ with numpy, soundfile (libsndfile with FLAC), mutagen and
Pillow. The default output is `library/` next to this file (gitignored: audio
is never committed); `library.manifest.json` next to it is committed and
holds every file's SHA-256, so a regenerated library can be checked against it.

Deterministic: seeded numpy PCM (quantised to int16 before encoding), fixed
tag order, fixed FLAC padding, Pillow JPEG with fixed parameters, and every
file/dir mtime pinned to FIXED_MTIME. Re-running with the same libsndfile /
libFLAC / mutagen / Pillow versions yields byte-identical files; the manifest
records those versions plus a `library_digest` over (path, sha256) pairs.

The library is built in a sibling `.tmp` dir and swapped in at the end; an
existing output that does not match its manifest is only replaced with --force.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
from datetime import datetime, timezone
from io import BytesIO
from pathlib import Path
from typing import Any, Callable

import mutagen
import numpy as np
import PIL
import soundfile
from mutagen.flac import FLAC, Picture
from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import albums as spec  # noqa: E402  (the spec next to this file)

LIBRARY_SRC = HERE / "library"
LIBRARY_MANIFEST = HERE / "library.manifest.json"


def sha256_file(path: Path, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


GENERATOR_VERSION = f"gen_fixture_library/1 spec/{spec.GENERATOR_SPEC_VERSION}"
FIXED_MTIME = 1704067200  # 2024-01-01T00:00:00Z — pinned so scanner dir_mtime is stable
FLAC_PADDING = 1024
COVER_SIZE = 600
JPEG_QUALITY = 90
APPLEDOUBLE_BYTES = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x00"

# Tag write order (anything else follows in spec order).
_TAG_ORDER = ["TITLE", "ARTIST", "ALBUM", "ALBUMARTIST", "DATE", "GENRE", "TRACKNUMBER",
              "TRACKTOTAL", "DISCNUMBER", "DISCTOTAL", "ARTISTSORT", "ALBUMARTISTSORT"]


# ---------- audio ----------

def make_pcm(seed: int) -> np.ndarray:
    """10 s stereo int16. Per-track base frequency (pitch class from seed*7 mod 12,
    so consecutive seeds never share a pitch class), stepping through a 4-note
    seeded pattern every 2.5 s, 3 harmonics, slow amplitude envelope, low noise."""
    rng = np.random.default_rng(seed)
    n = int(spec.SAMPLE_RATE * spec.DURATION_S)
    t = np.arange(n, dtype=np.float64) / spec.SAMPLE_RATE
    semitone = (seed * 7) % 12 + 12 * ((seed // 12) % 2)
    f0 = 110.0 * 2.0 ** (semitone / 12.0)
    ratios = np.array([1.0, 9 / 8, 5 / 4, 4 / 3, 3 / 2, 5 / 3])
    pattern = np.concatenate([[1.0], rng.choice(ratios, size=3)])
    seg = np.minimum((t / (spec.DURATION_S / 4)).astype(int), 3)
    inst_f = f0 * pattern[seg]
    phase = 2 * np.pi * np.cumsum(inst_f) / spec.SAMPLE_RATE
    amps = np.array([0.5, 0.25 + 0.1 * rng.random(), 0.1 + 0.1 * rng.random()])
    sig = sum(a * np.sin((h + 1) * phase) for h, a in enumerate(amps))
    env = 0.65 + 0.35 * np.sin(2 * np.pi * (0.1 + 0.05 * rng.random()) * t + rng.random() * 2 * np.pi)
    fade = np.minimum(1.0, np.minimum(t / 0.05, (spec.DURATION_S - t) / 0.05))
    base = 0.45 * sig * env * fade
    left = base + 0.002 * rng.standard_normal(n)
    right = 0.9 * base + 0.002 * rng.standard_normal(n)
    stereo = np.stack([left, right], axis=1)
    return np.clip(np.round(stereo * 32767.0), -32768, 32767).astype(np.int16)


# ---------- cover ----------

def make_cover_jpeg(dir_name: str, initials: str) -> bytes:
    key = hashlib.sha1(spec.nfc(dir_name).encode("utf-8")).digest()
    c1 = np.array(list(key[0:3]), dtype=np.float64)
    c2 = np.array(list(key[3:6]), dtype=np.float64)
    yy, xx = np.mgrid[0:COVER_SIZE, 0:COVER_SIZE]
    w = ((xx + yy) / (2 * (COVER_SIZE - 1)))[..., None]
    arr = (c1 * (1 - w) + c2 * w).round().astype(np.uint8)
    img = Image.fromarray(arr, "RGB")
    draw = ImageDraw.Draw(img)
    font = ImageFont.load_default(size=220)
    draw.text((COVER_SIZE / 2, COVER_SIZE / 2), initials, fill=(255, 255, 255), font=font,
              anchor="mm", stroke_width=6, stroke_fill=(0, 0, 0))
    buf = BytesIO()
    img.save(buf, format="JPEG", quality=JPEG_QUALITY, optimize=False, progressive=False)
    return buf.getvalue()


def initials_of(album: dict[str, Any]) -> str:
    title = spec.nfc(str(album["tags"].get("ALBUM", "?")))
    artist = spec.nfc(str(album["tags"].get("ALBUMARTIST") or album["tags"].get("ARTIST", "?")))
    return (artist[:1] + title[:1]).upper()


# ---------- tags ----------

def track_tags(album: dict[str, Any], t: dict[str, Any]) -> list[tuple[str, str]]:
    merged: dict[str, Any] = dict(album["tags"])
    merged["TITLE"] = t["title"]
    if t.get("tracknumber") is not None:
        merged["TRACKNUMBER"] = t["tracknumber"]
    merged["TRACKTOTAL"] = str(len(album["tracks"]))
    merged.update(t.get("extra_tags", {}))
    order = [k for k in _TAG_ORDER if k in merged] + [k for k in merged if k not in _TAG_ORDER]
    out: list[tuple[str, str]] = []
    for k in order:
        v = merged[k]
        if v is None:
            continue
        for item in (v if isinstance(v, list) else [v]):
            out.append((k, str(item)))
    return out


def write_flac(path: Path, pcm: np.ndarray, tags: list[tuple[str, str]],
               cover: bytes | None) -> None:
    soundfile.write(str(path), pcm, spec.SAMPLE_RATE, subtype="PCM_16", format="FLAC")
    audio = FLAC(str(path))
    if audio.tags is None:
        audio.add_tags()
    else:
        audio.tags.clear()
    audio.clear_pictures()
    for k, v in tags:
        audio.tags.append((k, v))  # type: ignore[union-attr]
    if cover is not None:
        pic = Picture()
        pic.type = 3
        pic.mime = "image/jpeg"
        pic.desc = ""
        pic.width = COVER_SIZE
        pic.height = COVER_SIZE
        pic.depth = 24
        pic.data = cover
        audio.add_picture(pic)
    audio.save(padding=lambda info: FLAC_PADDING)


# ---------- library ----------

def build(out: Path) -> list[dict[str, Any]]:
    """Write every album into `out` (must not exist). Returns manifest album entries."""
    spec.validate()
    out.mkdir(parents=True)
    entries: list[dict[str, Any]] = []
    for album in spec.ALBUMS:
        adir = out / album["dir_name"]
        adir.mkdir()
        cover_mode = album.get("cover")
        cover = make_cover_jpeg(album["dir_name"], initials_of(album)) if cover_mode else None
        embed = cover if cover_mode in ("embedded", "both") else None
        pcm_by_file: dict[str, np.ndarray] = {}
        for t in album["tracks"]:
            pcm = pcm_by_file[t["dup_of"]] if "dup_of" in t else make_pcm(t["audio_seed"])
            pcm_by_file[t["filename"]] = pcm
            write_flac(adir / t["filename"], pcm, track_tags(album, t), embed)
        if cover_mode in ("file", "both"):
            (adir / album.get("cover_filename", "cover.jpg")).write_bytes(cover)  # type: ignore[arg-type]
        for x in album.get("extra_files", []):
            data = APPLEDOUBLE_BYTES if x["kind"] == "appledouble" else x["content"].encode("utf-8")
            (adir / x["filename"]).write_bytes(data)
        entries.append(album)
    return entries


def pin_mtimes(root: Path) -> None:
    for dirpath, dirnames, filenames in os.walk(root, topdown=False):
        for name in filenames:
            os.utime(Path(dirpath) / name, (FIXED_MTIME, FIXED_MTIME))
        os.utime(dirpath, (FIXED_MTIME, FIXED_MTIME))


def file_kind(p: Path) -> str:
    if p.name.startswith("._"):
        return "appledouble-junk"
    if p.suffix.lower() == ".flac":
        return "flac"
    if p.suffix.lower() in (".jpg", ".jpeg", ".png"):
        return "cover"
    return "other"


def describe(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    files: list[dict[str, Any]] = []
    for p in sorted(root.rglob("*")):
        if not p.is_file():
            continue
        rel = p.relative_to(root).as_posix()
        entry: dict[str, Any] = {
            "path": rel,
            "kind": file_kind(p),
            "sha256": sha256_file(p),
            "size": p.stat().st_size,
        }
        if entry["kind"] == "flac":
            info = FLAC(str(p)).info
            entry["md5_signature"] = f"{info.md5_signature:032x}"
            entry["sample_rate"] = info.sample_rate
            entry["channels"] = info.channels
            entry["bits_per_sample"] = info.bits_per_sample
            entry["total_samples"] = info.total_samples
        files.append(entry)

    albums_out: list[dict[str, Any]] = []
    for a in spec.ALBUMS:
        albums_out.append({
            "dir_name": a["dir_name"],
            "dir_name_nfc": spec.nfc(a["dir_name"]),
            "dir_name_is_nfc": spec.nfc(a["dir_name"]) == a["dir_name"],
            "track_count": len(a["tracks"]),
            "tracks": [
                {k: t[k] for k in ("filename", "title", "tracknumber") }
                | ({"dup_of": t["dup_of"]} if "dup_of" in t else {"audio_seed": t["audio_seed"]})
                for t in a["tracks"]
            ],
            "cover": a.get("cover"),
            "cover_filename": a.get("cover_filename", "cover.jpg") if a.get("cover") in ("file", "both") else None,
            "extra_files": [x["filename"] for x in a.get("extra_files", [])],
            "defects": a["defects"],
            "expected_checks": a["expected_checks"],
            "notes": a["notes"],
        })
    return albums_out, files


def library_digest(files: list[dict[str, Any]]) -> str:
    lines = "".join(f"{f['path']}\t{f['sha256']}\n" for f in files)
    return sha256_bytes(lines.encode("utf-8"))


def _is_pristine(out: Path, manifest_path: Path) -> bool:
    """True when `out` holds exactly the file set of the previous manifest."""
    if not manifest_path.is_file():
        return False
    try:
        prev = {f["path"] for f in read_json(manifest_path)["files"]}
    except Exception:
        return False
    cur = {p.relative_to(out).as_posix() for p in out.rglob("*") if p.is_file()}
    return prev == cur


def main(argv: list[str] | None = None, check_out: Callable[[Path], Path] | None = None) -> int:
    """`check_out` lets a caller fence the output directory (it returns the
    resolved path or raises)."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", type=Path, default=LIBRARY_SRC,
                    help="library dir (default: library/ next to this file)")
    ap.add_argument("--force", action="store_true",
                    help="replace --out even if it differs from the last manifest")
    args = ap.parse_args(argv)

    out = (check_out or (lambda p: p.expanduser().resolve()))(args.out)
    if out == HERE or out == Path(out.anchor) or out == Path.home().resolve():
        print(f"refusing: --out {out} must be a dedicated directory", file=sys.stderr)
        return 2
    manifest_path = LIBRARY_MANIFEST if out == LIBRARY_SRC.resolve() else out.parent / f"{out.name}.manifest.json"

    if out.exists() and not args.force and not _is_pristine(out, manifest_path):
        print(f"refusing: {out} exists and does not match {manifest_path}; pass --force", file=sys.stderr)
        return 2

    tmp = out.parent / f".{out.name}.tmp"
    if tmp.exists():
        shutil.rmtree(tmp)
    build(tmp)
    pin_mtimes(tmp)
    if out.exists():
        shutil.rmtree(out)
    tmp.rename(out)

    albums_out, files = describe(out)
    digest = library_digest(files)
    manifest = {
        "generator_version": GENERATOR_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "library_digest": digest,
        "fixed_mtime": FIXED_MTIME,
        "audio": {"sample_rate": spec.SAMPLE_RATE, "channels": spec.CHANNELS,
                  "duration_s": spec.DURATION_S, "subtype": "PCM_16", "format": "FLAC",
                  "flac_padding": FLAC_PADDING},
        "tool_versions": {
            "libsndfile": soundfile.__libsndfile_version__,
            "soundfile": soundfile.__version__,
            "mutagen": mutagen.version_string,
            "numpy": np.__version__,
            "Pillow": PIL.__version__,
            "python": sys.version.split()[0],
        },
        "albums": albums_out,
        "files": files,
        "total_size": sum(f["size"] for f in files),
    }
    write_json(manifest_path, manifest)
    print(f"wrote {len(files)} files ({manifest['total_size'] / 1e6:.1f} MB) in "
          f"{len(albums_out)} albums -> {out}")
    print(f"manifest {manifest_path}  library_digest {digest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
