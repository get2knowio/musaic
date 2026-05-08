"""Hermetic test setup.

`MUSAIC_CONFIG_DIR` is captured at import time inside
`musaic.config`, so the env var MUST be set before any
`musaic.*` import. Pytest loads conftest.py before any test
file, so the assignment at the top of this file runs early enough.
"""
from __future__ import annotations

import os
import shutil
import tempfile

# Single per-session tmp dir for the app's config + SQLite + thumbs.
# Tests treat this as shared state and namespace their fixtures so they
# don't collide.
_TMP_CONFIG = tempfile.mkdtemp(prefix="musaic-test-cfg-")
os.environ["MUSAIC_CONFIG_DIR"] = _TMP_CONFIG

import pytest
from fastapi.testclient import TestClient


def pytest_sessionfinish(session, exitstatus):
    """Clean up the tmp config dir at the end of the run."""
    shutil.rmtree(_TMP_CONFIG, ignore_errors=True)


@pytest.fixture(scope="session")
def client() -> TestClient:
    """A TestClient over the real FastAPI app.

    Lifespan is intentionally NOT entered (no `with`-block) so the AI
    worker doesn't start during tests — we only exercise the request
    layer here.
    """
    from musaic.main import app
    return TestClient(app)


@pytest.fixture
def empty_library(tmp_path):
    """A fresh empty directory the API can adopt as the library root."""
    lib = tmp_path / "library"
    lib.mkdir()
    return lib
