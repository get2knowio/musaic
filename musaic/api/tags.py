"""User-defined track tags + the tag vocabulary the editor surfaces.
Distinct from the FLAC GENRE field — these are stored per-track in the
cache + a sidecar JSON in each album dir; album-level views compute the
intersection."""
from __future__ import annotations

from typing import Any

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel

from .. import cache as cache_module
from ..services.albums import (
    list_album_filenames,
    persist_track_tags_sidecar,
    resolve_album_dict,
    resync_sidecars_for_albums,
    track_count_for,
)

router = APIRouter()


# ---------- Track / album tag payloads ----------


class TrackTagPayload(BaseModel):
    tag: str


class AlbumTagsReplacePayload(BaseModel):
    """Wholesale replace of the album's track tags. Map keyed by track
    filename → list of tag strings."""
    track_tags: dict[str, list[str]]


class AlbumTagApplyPayload(BaseModel):
    """Apply a tag to the entire album. `filenames` may be omitted, in
    which case the server fans out to every FLAC in the album dir."""
    tag: str
    filenames: list[str] | None = None


# ---------- Track / album tag endpoints ----------


@router.get("/api/albums/{album_id}/tags")
def get_album_tags(album_id: str) -> dict[str, Any]:
    """Return the album's full tag state.

    Shape:
        {
            "album_id": "...",
            "track_tags":   {filename: [tag, ...], ...},
            "album_tags":   [tag, ...]   # intersection — tags on every track
        }
    """
    album = resolve_album_dict(album_id)
    track_tags = cache_module.get_album_track_tags(album_id)
    track_count = track_count_for(album)
    album_tags = cache_module.get_album_tag_intersection(album_id, track_count)
    return {
        "album_id": album_id,
        "track_tags": track_tags,
        "album_tags": album_tags,
    }


@router.put("/api/albums/{album_id}/tags")
def replace_album_tags(album_id: str, payload: AlbumTagsReplacePayload) -> dict[str, Any]:
    """Replace the album's track tags wholesale. Used by the bulk-edit
    dialog (e.g. paste a JSON snapshot to migrate from flac-jacket).
    The sidecar is rewritten to match."""
    album = resolve_album_dict(album_id)
    cache_module.replace_album_track_tags(album_id, payload.track_tags or {})
    persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@router.post("/api/albums/{album_id}/tags")
def add_album_tag(album_id: str, payload: AlbumTagApplyPayload) -> dict[str, Any]:
    """Apply `tag` to every track on the album (or the subset given in
    `filenames`). Idempotent — already-tagged tracks are left alone."""
    album = resolve_album_dict(album_id)
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    filenames = payload.filenames or list_album_filenames(album["path"])
    if not filenames:
        raise HTTPException(status_code=409, detail="album has no tracks")
    cache_module.add_album_tag(album_id, tag, filenames)
    persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@router.delete("/api/albums/{album_id}/tags/{tag}")
def remove_album_tag(album_id: str, tag: str) -> dict[str, Any]:
    """Strip `tag` from every track on the album."""
    album = resolve_album_dict(album_id)
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.remove_album_tag(album_id, tag)
    persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@router.post("/api/albums/{album_id}/tracks/{filename}/tags")
def add_one_track_tag(album_id: str, filename: str, payload: TrackTagPayload) -> dict[str, Any]:
    """Add a tag to a single track. Filename is the FLAC's basename
    (URL-encode special characters at the call site)."""
    album = resolve_album_dict(album_id)
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_track_tag(album_id, filename, tag)
    persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@router.delete("/api/albums/{album_id}/tracks/{filename}/tags/{tag}")
def remove_one_track_tag(album_id: str, filename: str, tag: str) -> dict[str, Any]:
    """Remove a tag from a single track."""
    album = resolve_album_dict(album_id)
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.remove_track_tag(album_id, filename, tag)
    persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@router.get("/api/tags")
def list_tags() -> dict[str, Any]:
    """All tags the user has ever applied, with usage counts. Powers
    the autocomplete dropdown in the tag editor."""
    return {"tags": cache_module.list_all_tags()}


@router.get("/api/tags/{tag}/tracks")
def tracks_for_tag(tag: str) -> dict[str, Any]:
    """Every (album_id, filename) pair carrying `tag`. The
    playlist-construction query."""
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    return {"tag": tag, "tracks": cache_module.find_tracks_with_tag(tag)}


# ---------- Tag vocabulary ----------
#
# The user-curated approved list of tags that powers the picker UIs in
# the album/track tag editors. See cache.py vocabulary helpers for the
# semantics; this layer is mostly thin REST + sidecar resync orchestration
# after any mutation that touches track_tags.


class VocabularyAddPayload(BaseModel):
    tag: str
    description: str = ""
    color: str | None = None
    sort_order: int | None = None


class VocabularyPatchPayload(BaseModel):
    description: str | None = None
    color: str | None = None
    sort_order: int | None = None


class VocabularyRenamePayload(BaseModel):
    new_tag: str


class VocabularyMergePayload(BaseModel):
    into_tag: str


@router.get("/api/vocabulary")
def list_vocabulary() -> dict[str, Any]:
    """The full vocabulary plus orphans (tags in track_tags but not in
    the vocabulary). The Settings panel renders both."""
    return {
        "vocabulary": cache_module.list_vocabulary(),
        "orphans": cache_module.find_orphan_tags(),
    }


@router.post("/api/vocabulary")
def add_vocabulary_entry(payload: VocabularyAddPayload) -> dict[str, Any]:
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_vocabulary_entry(
        tag,
        description=payload.description or "",
        color=payload.color,
        sort_order=payload.sort_order,
    )
    return list_vocabulary()


@router.patch("/api/vocabulary/{tag}")
def patch_vocabulary_entry(tag: str, payload: VocabularyPatchPayload) -> dict[str, Any]:
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.update_vocabulary_entry(
        tag,
        description=payload.description,
        color=payload.color,
        sort_order=payload.sort_order,
    )
    return list_vocabulary()


@router.post("/api/vocabulary/{tag}/rename")
def rename_vocabulary_entry(tag: str, payload: VocabularyRenamePayload) -> dict[str, Any]:
    tag = (tag or "").strip()
    new_tag = (payload.new_tag or "").strip()
    if not tag or not new_tag:
        raise HTTPException(status_code=400, detail="tag and new_tag are required")
    try:
        result = cache_module.rename_vocabulary_entry(tag, new_tag)
    except ValueError as e:
        raise HTTPException(status_code=409, detail=str(e))
    resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "rename_result": result}


@router.post("/api/vocabulary/{tag}/merge")
def merge_vocabulary_entry(tag: str, payload: VocabularyMergePayload) -> dict[str, Any]:
    """Merge `tag` into `payload.into_tag`. Both must exist before the
    call (the destination is the survivor)."""
    tag = (tag or "").strip()
    into_tag = (payload.into_tag or "").strip()
    if not tag or not into_tag:
        raise HTTPException(status_code=400, detail="tag and into_tag are required")
    try:
        result = cache_module.merge_vocabulary_entries(tag, into_tag)
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "merge_result": result}


@router.delete("/api/vocabulary/{tag}")
def delete_vocabulary_entry(
    tag: str,
    strip_from_tracks: bool = Query(False, description="Also delete every track_tags row carrying this tag"),
) -> dict[str, Any]:
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    try:
        result = cache_module.delete_vocabulary_entry(
            tag, strip_from_tracks=strip_from_tracks,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    if strip_from_tracks:
        resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "delete_result": result}


@router.post("/api/vocabulary/orphans/{tag}/promote")
def promote_orphan_tag(tag: str) -> dict[str, Any]:
    """Add an orphan tag to the vocabulary (no track_tags changes).
    Convenience over POST /api/vocabulary — preserves the exact tag
    string the user has already been using on tracks."""
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_vocabulary_entry(tag)
    return list_vocabulary()
