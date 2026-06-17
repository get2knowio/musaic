"""Tests for provider selection + generic API-key injection in ai.py.

The migration lets a user pick any airframe provider/model and supply one key.
The key is injected into the adapter constructor only when that adapter accepts
an `api_key=` — adapters with their own env/CLI auth chains are built bare. SDK-
free: nothing here spawns a vendor subprocess (constructing a runtime only
stores config).
"""
from __future__ import annotations

from musaic import ai


def test_runtime_accepts_api_key_by_provider():
    # Claude takes a pasted key; introspected from the constructor signature.
    assert ai.runtime_accepts_api_key("claude") is True
    # Bedrock uses the AWS chain — no api_key arg (also not installed, which
    # the helper swallows to False either way).
    assert ai.runtime_accepts_api_key("bedrock") is False
    # Unknown provider id never raises — just False.
    assert ai.runtime_accepts_api_key("not-a-provider") is False


def test_build_runtime_injects_key_when_supported():
    from claude_agent_sdk import ClaudeSDKClient  # noqa: F401 — ensures SDK importable

    rt = ai._build_runtime("claude", "sk-test-abc")
    # ClaudeCodeRuntime stashes the explicit key as an override.
    assert rt._api_key_override == "sk-test-abc"


def test_build_runtime_omits_empty_key():
    rt = ai._build_runtime("claude", "")
    assert rt._api_key_override is None
