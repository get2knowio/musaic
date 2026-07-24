"""Phase 2 (issue #8): bundled-binary path resolution in fingerprint.py.

The desktop bundle ships its own `fpcalc` + `libchromaprint` and points the
backend at them via env vars (no Homebrew/distro install on the user's
machine). These tests pin the resolution precedence hermetically — they never
execute fpcalc or load the real dylib, only assert which path each resolver
would pick.
"""
from __future__ import annotations

from musaic import fingerprint as fp

# ---------- fpcalc binary resolution ----------

def _make_exe(tmp_path, name):
    """Create a fake executable file and return its path."""
    p = tmp_path / name
    p.write_text("#!/bin/sh\nexit 0\n")
    p.chmod(0o755)
    return str(p)


def test_resolve_fpcalc_prefers_musaic_env(tmp_path, monkeypatch):
    bundled = _make_exe(tmp_path, "fpcalc-bundled")
    other = _make_exe(tmp_path, "fpcalc-other")
    monkeypatch.setenv("MUSAIC_FPCALC", bundled)
    monkeypatch.setenv("FPCALC", other)
    monkeypatch.setattr(fp.shutil, "which", lambda _: "/usr/bin/fpcalc")
    assert fp._resolve_fpcalc() == bundled


def test_resolve_fpcalc_falls_back_to_fpcalc_env(tmp_path, monkeypatch):
    other = _make_exe(tmp_path, "fpcalc-other")
    monkeypatch.delenv("MUSAIC_FPCALC", raising=False)
    monkeypatch.setenv("FPCALC", other)
    monkeypatch.setattr(fp.shutil, "which", lambda _: "/usr/bin/fpcalc")
    assert fp._resolve_fpcalc() == other


def test_resolve_fpcalc_falls_back_to_path(monkeypatch):
    monkeypatch.delenv("MUSAIC_FPCALC", raising=False)
    monkeypatch.delenv("FPCALC", raising=False)
    monkeypatch.setattr(fp.shutil, "which", lambda _: "/opt/homebrew/bin/fpcalc")
    assert fp._resolve_fpcalc() == "/opt/homebrew/bin/fpcalc"


def test_resolve_fpcalc_ignores_missing_or_nonexec_env(tmp_path, monkeypatch):
    # A non-executable / nonexistent env value is skipped, not trusted blindly.
    not_exec = tmp_path / "fpcalc-noexec"
    not_exec.write_text("x")
    not_exec.chmod(0o644)
    monkeypatch.setenv("MUSAIC_FPCALC", str(not_exec))
    monkeypatch.setenv("FPCALC", str(tmp_path / "does-not-exist"))
    monkeypatch.setattr(fp.shutil, "which", lambda _: "/usr/bin/fpcalc")
    assert fp._resolve_fpcalc() == "/usr/bin/fpcalc"


def test_resolve_fpcalc_returns_none_when_nothing_found(monkeypatch):
    monkeypatch.delenv("MUSAIC_FPCALC", raising=False)
    monkeypatch.delenv("FPCALC", raising=False)
    monkeypatch.setattr(fp.shutil, "which", lambda _: None)
    assert fp._resolve_fpcalc() is None


def test_is_available_reports_missing_binary(monkeypatch):
    monkeypatch.delenv("MUSAIC_FPCALC", raising=False)
    monkeypatch.delenv("FPCALC", raising=False)
    monkeypatch.setattr(fp.shutil, "which", lambda _: None)
    ok, reason = fp.is_available()
    assert ok is False
    assert "fpcalc" in reason


def test_is_available_ok_via_bundled_binary(tmp_path, monkeypatch):
    bundled = _make_exe(tmp_path, "fpcalc")
    monkeypatch.setenv("MUSAIC_FPCALC", bundled)
    monkeypatch.setattr(fp.shutil, "which", lambda _: None)  # no PATH install
    monkeypatch.setattr(fp.settings_module, "acoustid_api_key", lambda: "key")
    ok, reason = fp.is_available()
    assert ok is True
    assert reason == "ok"


# ---------- libchromaprint dylib resolution ----------

def test_libchromaprint_candidates_bundled_first(monkeypatch):
    monkeypatch.setenv("MUSAIC_LIBCHROMAPRINT", "/bundle/lib/libchromaprint.dylib")
    monkeypatch.setattr(fp.ctypes.util, "find_library", lambda _: None)
    candidates, _name = fp._libchromaprint_candidates()
    assert candidates[0] == "/bundle/lib/libchromaprint.dylib"
    # The hardcoded Homebrew/distro fallbacks still trail it.
    assert candidates[-len(fp._LIBCHROMAPRINT_CANDIDATES):] == list(fp._LIBCHROMAPRINT_CANDIDATES)


def test_libchromaprint_candidates_no_env(monkeypatch):
    monkeypatch.delenv("MUSAIC_LIBCHROMAPRINT", raising=False)
    monkeypatch.setattr(fp.ctypes.util, "find_library", lambda _: None)
    candidates, name = fp._libchromaprint_candidates()
    assert name is None
    assert candidates == list(fp._LIBCHROMAPRINT_CANDIDATES)


def test_libchromaprint_candidates_find_library_after_bundled(monkeypatch):
    monkeypatch.setenv("MUSAIC_LIBCHROMAPRINT", "/bundle/lib/libchromaprint.dylib")
    monkeypatch.setattr(fp.ctypes.util, "find_library", lambda _: "libchromaprint.so.1")
    candidates, name = fp._libchromaprint_candidates()
    assert name == "libchromaprint.so.1"
    assert candidates[0] == "/bundle/lib/libchromaprint.dylib"
    assert candidates[1] == "libchromaprint.so.1"
