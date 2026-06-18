"""Phase 5 (issue #8): AI provider runtime-readiness detection.

A packaged desktop app may not have the `claude` / `gh` CLI the selected
provider's adapter shells out to. `provider_runtime_status` reports that up
front so Settings can show an actionable banner and the worker can fail with a
remedy instead of crashing opaquely — while deterministic MusicBrainz matching
keeps working regardless.
"""
from __future__ import annotations

from musaic import ai


def test_claude_ready_when_cli_on_path(monkeypatch):
    monkeypatch.delenv("MUSAIC_CLAUDE_CLI", raising=False)
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: "/usr/local/bin/claude")
    status = ai.provider_runtime_status("claude")
    assert status["ready"] is True
    assert status["remedy"] is None
    assert "claude" in status["requirement"]


def test_claude_not_ready_when_cli_missing(monkeypatch):
    monkeypatch.delenv("MUSAIC_CLAUDE_CLI", raising=False)
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: None)
    status = ai.provider_runtime_status("claude")
    assert status["ready"] is False
    assert status["requirement"] == "the `claude` CLI"
    assert "Claude CLI" in status["remedy"]
    # The remedy reassures that the app still works without AI.
    assert "MusicBrainz" in status["remedy"]


def test_copilot_probes_gh_cli(monkeypatch):
    monkeypatch.delenv("MUSAIC_GH_CLI", raising=False)
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: None)
    status = ai.provider_runtime_status("github-copilot")
    assert status["ready"] is False
    assert status["requirement"] == "the `gh` CLI"


def test_key_or_unknown_provider_reports_ready(monkeypatch):
    # Providers with no external-CLI prerequisite are always "ready" here; the
    # real auth check happens at call time.
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: None)
    for pid in ("kimi", "openai-compatible", "bedrock", "totally-unknown"):
        status = ai.provider_runtime_status(pid)
        assert status == {"ready": True, "requirement": None, "remedy": None}


def test_cli_env_override_takes_precedence(tmp_path, monkeypatch):
    fake = tmp_path / "claude"
    fake.write_text("#!/bin/sh\n")
    fake.chmod(0o755)
    monkeypatch.setenv("MUSAIC_CLAUDE_CLI", str(fake))
    # PATH has no claude, but the explicit override is honored.
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: None)
    status = ai.provider_runtime_status("claude")
    assert status["ready"] is True


def test_cli_env_override_ignored_when_not_executable(tmp_path, monkeypatch):
    notexec = tmp_path / "claude"
    notexec.write_text("x")
    notexec.chmod(0o644)
    monkeypatch.setenv("MUSAIC_CLAUDE_CLI", str(notexec))
    monkeypatch.setattr(ai.shutil, "which", lambda cmd: None)
    status = ai.provider_runtime_status("claude")
    assert status["ready"] is False
