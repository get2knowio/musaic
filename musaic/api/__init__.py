"""FastAPI routers per concern. main.py imports these and includes them
on the app. No app-level state lives here; shared helpers belong in
`musaic.services`."""
from . import analyze, apply, health, jobs, library, settings, tags

__all__ = ["analyze", "apply", "health", "jobs", "library", "settings", "tags"]
