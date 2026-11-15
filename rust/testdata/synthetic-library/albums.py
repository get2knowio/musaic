"""Declarative spec for the synthetic fixture library.

Pure data + tiny helpers. `gen_fixture_library.py` (next to this file) turns
this into FLACs under `library/` and records what it wrote in
`library.manifest.json`.

Album dict shape:
    dir_name        on-disk directory name, exactly as written (may be NFD)
    tags            album-level Vorbis comments applied to every track
                    (value: str or list[str] for multi-value fields)
    tracks          [{filename, title, tracknumber, extra_tags, audio_seed | dup_of}]
                    extra_tags override album tags key-by-key (e.g. per-track ARTIST)
                    dup_of = filename of an earlier track in the same album whose
                    PCM samples are reused verbatim (identical audio, different tags)
    cover           "file" | "embedded" | "both" | None
    cover_filename  sidecar name when cover is "file"/"both" (default "cover.jpg")
    extra_files     [{filename, kind: "appledouble" | "text", content?}] non-audio junk
    defects         deliberate defects / edge cases (check ids where one applies)
    expected_checks check ids we expect to fire for this album in OFFLINE mode
                    (no MB anchor, no AcoustID). unidentified-album fires for
                    every album offline and is not repeated here.
    notes           free text

Defect vocabulary:
    no-cover-art, track-gaps, duplicate-tracks, inconsistent-artist-sort,
    filename-mismatch, tags-out-of-sync           -> the check of that id
    missing-albumartist      ARTIST only, no ALBUMARTIST (the "Mancini" cascade)
    compilation              Various Artists / per-track ARTIST
    multi-disc               "(disc N of M)" dir + DISCNUMBER/DISCTOTAL
    nonconforming-dir-name   parse_album_dir(...).matched is False
    nfd-dir-name             directory name stored in Unicode NFD, tags in NFC
    junk-files               AppleDouble `._*.flac` + non-audio file in album dir
    folder-jpg-cover         sidecar is folder.jpg, not cover.jpg
    multi-genre              GENRE differs / repeats across tracks (union + dedupe)
    article-artist           ALBUMARTIST starts with "The", no sort tags
"""
from __future__ import annotations

import unicodedata
from typing import Any

GENERATOR_SPEC_VERSION = "1"

# Shared audio format (generator enforces it).
SAMPLE_RATE = 44100
CHANNELS = 2
DURATION_S = 10.0


def nfd(s: str) -> str:
    return unicodedata.normalize("NFD", s)


def nfc(s: str) -> str:
    return unicodedata.normalize("NFC", s)


def track(n: int | None, title: str, seed: int | None = None, *, filename: str | None = None,
          dup_of: str | None = None, tracknumber: str | None = None,
          **extra_tags: Any) -> dict[str, Any]:
    """One track entry. Default filename follows the Python app's default
    single-artist template `{tracknumber:02} - {title}.flac`."""
    if filename is None:
        filename = f"{n:02d} - {title}.flac"
    t: dict[str, Any] = {
        "filename": filename,
        "title": title,
        "tracknumber": tracknumber if tracknumber is not None else (str(n) if n is not None else None),
        "extra_tags": {k.upper(): v for k, v in extra_tags.items()},
    }
    if dup_of is not None:
        t["dup_of"] = dup_of
    else:
        if seed is None:
            raise ValueError(f"track {filename!r} needs audio_seed or dup_of")
        t["audio_seed"] = seed
    return t


def album_tags(artist: str, album: str, date: str, genre: str | list[str], *,
               albumartist: str | None = None, artistsort: str | None = None,
               albumartistsort: str | None = None, **more: str) -> dict[str, Any]:
    tags: dict[str, Any] = {"ARTIST": artist, "ALBUM": album}
    if albumartist is not None:
        tags["ALBUMARTIST"] = albumartist
    tags["DATE"] = date
    tags["GENRE"] = genre
    if artistsort is not None:
        tags["ARTISTSORT"] = artistsort
    if albumartistsort is not None:
        tags["ALBUMARTISTSORT"] = albumartistsort
    tags.update({k.upper(): v for k, v in more.items()})
    return tags


AF = "Aurora Fields"
MH = "Mono Harbor"

ALBUMS: list[dict[str, Any]] = [
    {
        "dir_name": "Aurora Fields - First Light [FLAC]",
        "tags": album_tags(AF, "First Light", "2019-04-12", "Ambient", albumartist=AF,
                           artistsort=AF, albumartistsort=AF),
        "tracks": [
            track(1, "Dawn Chorus", 101),
            track(2, "Low Sun", 102),
            track(3, "Meridian", 103),
            track(4, "Long Shadows", 104),
            track(5, "Afterglow", 105),
        ],
        "cover": "both",
        "defects": [],
        "expected_checks": [],
        "notes": "Clean baseline. DATE is a full ISO date so the scanner's year "
                 "normalisation ('2019-04-12' -> '2019') is exercised. All Aurora Fields "
                 "albums share ALBUMARTISTSORT 'Aurora Fields' so their sort cluster is "
                 "consistent (no inconsistent-artist-sort finding).",
    },
    {
        "dir_name": "Aurora Fields - Second Light [FLAC]",
        "tags": album_tags(AF, "Second Light", "2021", "Ambient", albumartist=AF,
                           artistsort=AF, albumartistsort=AF),
        "tracks": [
            track(1, "Blue Hour", 111),
            track(2, "Cirrus", 112),
            track(3, "Halation", 113),
            track(4, "Nightfall", 114),
        ],
        "cover": None,
        "defects": ["no-cover-art"],
        "expected_checks": ["no-cover-art"],
        "notes": "No cover.jpg/folder.jpg/front.jpg and no embedded PICTURE block.",
    },
    {
        "dir_name": "Aurora Fields - Folder Art [FLAC]",
        "tags": album_tags(AF, "Folder Art", "2022", "Post-Rock", albumartist=AF,
                           artistsort=AF, albumartistsort=AF),
        "tracks": [
            track(1, "Canvas", 121),
            track(2, "Easel", 122),
        ],
        "cover": "file",
        "cover_filename": "folder.jpg",
        "defects": ["folder-jpg-cover"],
        "expected_checks": [],
        "notes": "Sidecar is folder.jpg only (COVER_FILENAMES preference order); "
                 "scanner should report cover_file='folder.jpg', has_embedded_cover=False.",
    },
    {
        "dir_name": "Aurora Fields - Twin (disc 1 of 2) [FLAC]",
        "tags": album_tags(AF, "Twin", "2023", "Electronic", albumartist=AF,
                           artistsort=AF, albumartistsort=AF,
                           discnumber="1", disctotal="2"),
        "tracks": [
            track(1, "Mirror", 131),
            track(2, "Parallax", 132),
            track(3, "Echo", 133),
        ],
        "cover": "embedded",
        "defects": ["multi-disc"],
        "expected_checks": [],
        "notes": "Disc 1 of a two-disc set; each disc is its own album row.",
    },
    {
        "dir_name": "Aurora Fields - Twin (disc 2 of 2) [FLAC]",
        "tags": album_tags(AF, "Twin", "2023", "Electronic", albumartist=AF,
                           artistsort=AF, albumartistsort=AF,
                           discnumber="2", disctotal="2"),
        "tracks": [
            track(1, "Reflection", 141),
            track(2, "Doppler", 142),
            track(3, "Return", 143),
        ],
        "cover": "embedded",
        "defects": ["multi-disc"],
        "expected_checks": [],
        "notes": "Disc 2 of a two-disc set; track numbers restart at 1.",
    },
    {
        "dir_name": "Mono Harbor - Gaps [FLAC]",
        "tags": album_tags(MH, "Gaps", "2016", "Indie Rock", artistsort=MH),
        "tracks": [
            track(1, "Pier One", 201),
            track(2, "Breakwater", 202),
            track(4, "Tidewater", 203),
            track(5, "Lighthouse", 204),
        ],
        "cover": "file",
        "defects": ["track-gaps", "missing-albumartist", "inconsistent-artist-sort"],
        "expected_checks": ["track-gaps", "inconsistent-artist-sort"],
        "notes": "TRACKNUMBER 1,2,4,5 (missing 3). ARTIST set, NO ALBUMARTIST: scanner "
                 "album_artist=None but artist='Mono Harbor' via the albumartist->artist "
                 "cascade. ARTISTSORT='Mono Harbor' (unsorted form) contributes a "
                 "TRACK-level member to the 'mono harbor' sort cluster (album-level "
                 "membership needs album_artist, which is empty).",
    },
    {
        "dir_name": "Mono Harbor - Sorted [FLAC]",
        "tags": album_tags(MH, "Sorted", "2018", "Indie Rock", albumartist=MH,
                           artistsort="Harbor, Mono", albumartistsort="Harbor, Mono"),
        "tracks": [
            track(1, "Ordered", 211),
            track(2, "Indexed", 212),
            track(3, "Filed", 213),
        ],
        "cover": "file",
        "defects": ["inconsistent-artist-sort"],
        "expected_checks": ["inconsistent-artist-sort"],
        "notes": "Sort tags 'Harbor, Mono' conflict with 'Mono Harbor' on Gaps -> the "
                 "'mono harbor' cluster has two distinct non-empty sort values = "
                 "state 'conflicting'. One finding per cluster (not per album).",
    },
    {
        "dir_name": "Mono Harbor - Dupes [FLAC]",
        "tags": album_tags(MH, "Dupes", "2019", "Shoegaze", albumartist=MH),
        "tracks": [
            track(1, "Static Bloom", 221),
            track(2, "Feedback Garden", 222),
            track(3, "Static Bloom (Reprise)", dup_of="01 - Static Bloom.flac"),
            track(4, "Wall of Sound", 223),
        ],
        "cover": "embedded",
        "defects": ["duplicate-tracks"],
        "expected_checks": ["duplicate-tracks"],
        "notes": "Track 3's PCM is identical to track 1 (same STREAMINFO md5_signature), "
                 "different filename/title. duplicate-tracks needs track_fingerprints rows "
                 "(fpcalc + the fingerprint pool), so it only fires once fingerprinting ran. "
                 "No sort tags: an empty member of the 'mono harbor' cluster.",
    },
    {
        "dir_name": "Mono Harbor - Dotfiles [FLAC]",
        "tags": album_tags(MH, "Dotfiles", "2020", "Lo-Fi", albumartist=MH),
        "tracks": [
            track(1, "Hidden", 231),
            track(2, "Visible", 232),
        ],
        "cover": "file",
        "extra_files": [
            {"filename": "._01 - Ghost.flac", "kind": "appledouble"},
            {"filename": "notes.txt", "kind": "text",
             "content": "Ripped from a dusty CD-R. Not audio; the scanner must ignore me.\n"},
        ],
        "defects": ["junk-files"],
        "expected_checks": [],
        "notes": "`._01 - Ghost.flac` is a few bytes of AppleDouble header (not valid FLAC) "
                 "and must be skipped by _list_flacs; notes.txt is skipped by extension. "
                 "Expected track_count=2.",
    },
    {
        "dir_name": "Various Artists - Night Mix [FLAC]",
        "tags": album_tags("Various Artists", "Night Mix", "2017", "Electronic",
                           compilation="1"),
        "tracks": [
            track(1, "Neon Tide", 301, filename="trackA.flac", artist="Lumen Drift"),
            track(2, "Something Electric", 302, filename="02 - something.flac",
                  artist="Sable & The Static", genre="House"),
            track(3, "Velvet Signal", 303, filename="Night Mix 3.flac",
                  artist="Kite Theory", genre=["Downtempo", "electronic"]),
            track(4, "Last Train", 304, filename="04.flac", artist="Orbit Society"),
        ],
        "cover": "file",
        "defects": ["compilation", "filename-mismatch", "multi-genre"],
        "expected_checks": ["filename-mismatch"],
        "notes": "Album-level ARTIST is overridden per track; no ALBUMARTIST. Filenames "
                 "match neither template ('{tracknumber:02} - {artist} - {title}' for "
                 "compilations). Scanner reads files in sorted() codepoint order "
                 "('02 - something', '04', 'Night Mix 3', 'trackA'), so the sample track is "
                 "'02 - something.flac': album artist='Sable & The Static', genre union = "
                 "'House; Electronic; Downtempo' (case-insensitive dedupe, first-seen casing), "
                 "track_numbers=[2, 4, 3, 1].",
    },
    {
        "dir_name": "Legacy Folder Name",
        "tags": album_tags("Quiet Atlas", "Old Rips", "2009", "Folk",
                           albumartist="Quiet Atlas"),
        "tracks": [
            track(1, "Kitchen Radio", 401),
            track(2, "Porch Light", 402),
        ],
        "cover": "file",
        "defects": ["nonconforming-dir-name", "filename-mismatch"],
        "expected_checks": ["filename-mismatch"],
        "notes": "Directory name does not match the convention: parse matched=False, "
                 "dir_artist/dir_album None, title falls back to the ALBUM tag. The "
                 "filename-mismatch dir template proposes 'Quiet Atlas - Old Rips [FLAC]'.",
    },
    {
        "dir_name": nfd("Café Orchestra - Été [FLAC]"),
        "dir_name_nfc": nfc("Café Orchestra - Été [FLAC]"),
        "tags": album_tags(nfc("Café Orchestra"), nfc("Été"), "2014", "Jazz",
                           albumartist=nfc("Café Orchestra")),
        "tracks": [
            track(1, nfc("Soirée"), 501),
            track(2, "Terrasse", 502),
            track(3, nfc("Crème"), 503),
        ],
        "cover": "both",
        "defects": ["nfd-dir-name"],
        "expected_checks": ["filename-mismatch"],
        "notes": "Directory name written in NFD; every tag (and track filenames) in NFC. "
                 "The Python app has no unicodedata normalisation anywhere: dir_album comes out NFD, "
                 "album id = sha1 of the NFD path, filename-mismatch compares an "
                 "NFD-derived rendering against the NFD dir name. Behaviour on a "
                 "normalising filesystem is an open unknown. OBSERVED (sanity run): _check_dir "
                 "renders albumartist from the NFC tag and album from the NFD dir parse, so "
                 "the proposed dir name is MIXED-normalisation and differs from the current "
                 "name only by normalisation -> a spurious filename-mismatch dir rename.",
    },
    {
        "dir_name": "The Lanterns - Porchlight [FLAC]",
        "tags": album_tags("The Lanterns", "Porchlight", "2012", "Americana",
                           albumartist="The Lanterns"),
        "tracks": [
            track(1, "Screen Door", 601),
            track(2, "Fireflies", 602),
            track(3, "Gravel Road", 603),
        ],
        "cover": "file",
        "defects": ["article-artist", "tags-out-of-sync"],
        "expected_checks": ["tags-out-of-sync"],
        "notes": "Added so tags-out-of-sync fires offline: with no MB anchor its only "
                 "triggers are blank core fields or article-rotation sort proposals; "
                 "ALBUMARTIST/ARTIST 'The Lanterns' with no sort tags proposes "
                 "'Lanterns, The'.",
    },
]


def album_by_dir(dir_name: str) -> dict[str, Any]:
    for a in ALBUMS:
        if a["dir_name"] == dir_name or a.get("dir_name_nfc") == dir_name:
            return a
    raise KeyError(dir_name)


def validate(albums: list[dict[str, Any]] = ALBUMS) -> None:
    """Cheap structural checks so a typo fails before any audio is written."""
    seen_dirs: set[str] = set()
    seen_seeds: set[int] = set()
    for a in albums:
        d = a["dir_name"]
        if nfc(d) in seen_dirs:
            raise ValueError(f"duplicate album dir {d!r}")
        seen_dirs.add(nfc(d))
        if a.get("cover") not in ("file", "embedded", "both", None):
            raise ValueError(f"{d}: bad cover {a.get('cover')!r}")
        names: set[str] = set()
        for t in a["tracks"]:
            if t["filename"] in names:
                raise ValueError(f"{d}: duplicate filename {t['filename']!r}")
            if "dup_of" in t and t["dup_of"] not in names:
                raise ValueError(f"{d}: dup_of {t['dup_of']!r} must name an earlier track")
            if "audio_seed" in t:
                if t["audio_seed"] in seen_seeds:
                    raise ValueError(f"{d}: audio_seed {t['audio_seed']} reused")
                seen_seeds.add(t["audio_seed"])
            names.add(t["filename"])
