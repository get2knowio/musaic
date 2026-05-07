"""Render filename templates for the filename-mismatch health check
and its rename apply path.

A template is a Python str.format-ish string with named placeholders:

    "{tracknumber:02} - {title}"
    "{tracknumber:02} - {artist} - {title}"
    "{disc}-{tracknumber:02} - {title}"   # multi-disc

Variables understood:

  - tracknumber  int  (rendered with format-spec, e.g. {tracknumber:02})
  - title        str
  - artist       str
  - albumartist  str
  - album        str
  - year         str
  - disc         int  (rendered with format-spec)

Missing values render as empty string rather than raising — that
keeps the check resilient on partially-tagged albums (the resulting
filename is just unusable, which the check shows as a mismatch).

The renderer is intentionally NOT Python's str.format directly:
str.format raises KeyError on missing variables and ValueError on
type-mismatch (passing a string where {x:02} expected an int). We
need it to be tolerant — health checks have to keep working on
real-world ill-tagged files. The custom implementation below maps
unknown / type-incompatible variables to empty strings.

Output is always passed through `sanitize_for_filename` to strip
filesystem-illegal characters (slashes, control chars, trailing dots
on Windows). The .flac extension is implicit — callers append it.
"""
from __future__ import annotations

import logging
import re
from typing import Any, Mapping

log = logging.getLogger(__name__)


# Format-spec types: ints get integer formatting (zero-pad, etc.),
# strings get string formatting (truncation, alignment). When the
# template asks for an integer format on a string value (rare but
# possible), we coerce by trying int(); if that fails, fall back to
# the bare string.
_INT_FIELDS: frozenset[str] = frozenset({"tracknumber", "disc"})

# Filesystem-unsafe characters.
#   /     path separator on every OS
#   \     path separator on Windows + sometimes a quote/escape on macOS
#   :     reserved on Windows + macOS (HFS legacy)
#   *?<>| Windows-reserved
#   "     Windows-reserved
#
# We replace with " - " (slash → " - ") rather than dropping outright,
# because some titles legitimately contain colons / slashes and the
# user wants them preserved as readable text. NUL and other control
# chars get stripped entirely.
_UNSAFE_CHARS_RE = re.compile(r'[/\\:*?"<>|]')
_CONTROL_CHARS_RE = re.compile(r"[\x00-\x1f\x7f]")
# Windows refuses trailing whitespace + dots on filenames. Even on
# Mac/Linux, a trailing dot reads as a hidden file's extension prefix
# in some tools. Strip them.
_TRAILING_BAD_RE = re.compile(r"[.\s]+$")


def render(template: str, values: Mapping[str, Any]) -> str:
    """Render `template` with `values`, sanitize, and return the
    base filename (no extension).

    Tolerant of:
      - missing variables in `values` (render as empty string)
      - format specs incompatible with the value type (fall through
        to bare-string rendering)
      - templates with no variables at all (renders as-is)
    """
    if not template:
        return ""

    def _replace(match: re.Match[str]) -> str:
        name = match.group("name")
        spec = match.group("spec") or ""
        raw = values.get(name)
        return _format_value(name, raw, spec)

    out = _PLACEHOLDER_RE.sub(_replace, template)
    return sanitize_for_filename(out)


# Matches {name} or {name:spec}. Names are letters/digits/underscores;
# specs are anything until the closing brace.
_PLACEHOLDER_RE = re.compile(r"\{(?P<name>[a-zA-Z_][a-zA-Z0-9_]*)(?::(?P<spec>[^}]*))?\}")


def _format_value(name: str, raw: Any, spec: str) -> str:
    """Render one variable, with spec-aware fallbacks. Returns "" for
    None / unset values so the resulting filename has a clean gap
    rather than a literal "None"."""
    if raw is None:
        return ""
    if name in _INT_FIELDS:
        # Coerce to int for {tracknumber:02} style. If the value
        # came in as a string ("05" or "5/12"), peel off the leading
        # number; otherwise drop to bare-string rendering.
        as_int = _coerce_int(raw)
        if as_int is not None:
            try:
                return format(as_int, spec) if spec else str(as_int)
            except (ValueError, TypeError):
                return str(as_int)
        # Couldn't get an int — fall through to bare string.
        return str(raw).strip()
    # String field. Apply the spec via Python's format() if present;
    # else just stringify.
    s = str(raw).strip()
    if spec:
        try:
            return format(s, spec)
        except (ValueError, TypeError):
            return s
    return s


def _coerce_int(raw: Any) -> int | None:
    """Best-effort int parse. Handles "5", "05", "5/12" (track 5 of 12),
    integers, floats."""
    if isinstance(raw, int):
        return raw
    if isinstance(raw, float):
        return int(raw)
    if not isinstance(raw, str):
        return None
    s = raw.strip()
    if not s:
        return None
    # "5/12" → "5"
    if "/" in s:
        s = s.split("/", 1)[0].strip()
    try:
        return int(s)
    except ValueError:
        return None


def sanitize_for_filename(name: str) -> str:
    """Make a string safe to use as a base filename across OSes.

    - Replace /, \\, :, *, ?, ", <, >, | with " - " each. Multiple
      consecutive bad chars collapse to a single " - " via the
      whitespace cleanup pass.
    - Strip control characters entirely.
    - Trim trailing whitespace + dots (Windows requirement; Plex
      handles them but other tools don't).
    - Collapse multiple spaces and stray dashes that the
      replacement may have created.

    Empty input returns empty string.
    """
    if not name:
        return ""
    s = _UNSAFE_CHARS_RE.sub(" - ", name)
    s = _CONTROL_CHARS_RE.sub("", s)
    # Collapse repeated whitespace.
    s = re.sub(r"\s+", " ", s).strip()
    # Collapse repeated " - " separators (e.g. "Title:?Sub" → " - " " - "
    # → "Title - Sub" once consecutive separators are merged).
    s = re.sub(r"(?:\s*-\s*){2,}", " - ", s)
    # Strip leading / trailing " - " sequences AND trailing whitespace
    # / dots (both Windows-illegal and visually noisy). The character
    # replacement above can leave a stranded separator at either end
    # when the input started or ended with an unsafe char.
    s = re.sub(r"^(?:\s*-\s*)+", "", s)
    s = re.sub(r"(?:\s*-\s*)+$", "", s)
    s = _TRAILING_BAD_RE.sub("", s)
    return s
