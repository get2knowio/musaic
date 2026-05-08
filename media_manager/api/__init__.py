"""FastAPI routers per concern. main.py imports these and includes them
on the app. No app-level state lives here; shared helpers belong in
`media_manager.services`."""
from . import apply, health, jobs, library, settings, tags

__all__ = ["apply", "health", "jobs", "library", "settings", "tags"]
