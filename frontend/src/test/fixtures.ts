/**
 * Test fixture builders. `makeAlbum` / `makeJob` return a fully-typed
 * baseline that satisfies the (large) Album/Job interfaces so individual
 * tests can override only the fields they care about.
 */
import type { Album, Job } from "@/lib/types";

export function makeAlbum(overrides: Partial<Album> = {}): Album {
  return {
    id: "album-1",
    path: "/music/Artist - Album [FLAC]",
    dir_name: "Artist - Album [FLAC]",
    title: "Album",
    artist: "Artist",
    album_artist: "Artist",
    year: "2001",
    genre: "Rock",
    track_count: 10,
    total_seconds: 2400,
    has_embedded_cover: true,
    cover_file: null,
    sample_track: "01 - Track.flac",
    dir_mtime: 1_700_000_000,
    dir_artist: "Artist",
    dir_album: "Album",
    is_compilation: false,
    disc_number: null,
    disc_total: null,
    sample_rate: 44_100,
    bits_per_sample: 16,
    channels: 2,
    album_artist_sort: null,
    artist_sort: null,
    album_sort: null,
    mb_release_group_mbid: null,
    mb_last_checked_at: null,
    metadata_ai_acted_on_at: null,
    missing_since: null,
    ...overrides,
  };
}

export function makeJob(overrides: Partial<Job> = {}): Job {
  return {
    id: "job-1",
    album_id: "album-1",
    album_path: "/music/Artist - Album [FLAC]",
    album_title: "Album",
    job_type: "suggest_metadata" as Job["job_type"],
    hint: "",
    status: "queued" as Job["status"],
    result: null,
    error: null,
    acted_on_action: null,
    created_at: "2026-01-01T00:00:00Z",
    started_at: null,
    completed_at: null,
    acted_on_at: null,
    ...overrides,
  };
}
