"""Musaic — web UI for browsing and managing a local music collection."""

# Eagerly load .env files so claude-agent-sdk and any other env-driven config
# picks up CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY without the user having
# to export them in their shell first.
from .dotenv import bootstrap_env as _bootstrap_env

_bootstrap_env()

__version__ = "0.1.0"
