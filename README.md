# Musaic

A local-first web app for browsing and maintaining a personal FLAC music collection, with AI-assisted metadata and cover art.

## What it does

Point Musaic at a directory of FLAC files. Any subdirectory containing FLACs is treated as an album. From there you can:

- Browse your library as an album grid or sortable list
- Open any album to see tracks, tags, embedded cover art, and open issues
- Ask Claude to suggest metadata corrections (title, artist, year, genre, sort tags, MBIDs) and canonical cover art — with a diff preview before anything writes
- Apply suggestions: Musaic writes Vorbis tags, optionally renames files, and embeds or saves cover art
- Run health checks across your library — missing tags, track gaps, filename mismatches, duplicate tracks, missing fingerprints, and more
- Queue bulk fixes and review them from the Jobs page

## Requirements

- Python 3.10+
- [uv](https://docs.astral.sh/uv/) (recommended) or pip
- Node.js 18+ (to build the frontend; a pre-built copy is included)
- Optional: `fpcalc` for audio fingerprinting (`brew install chromaprint`)

## Setup

```bash
uv sync
```

## Run

```bash
uv run media-manager
```

Open <http://127.0.0.1:8765>. Click the gear icon to configure your library directory.

## AI authentication

Claude features require credentials in the environment or a `.env` file in the project directory:

```
CLAUDE_CODE_OAUTH_TOKEN=...   # preferred if you have Claude Code installed
ANTHROPIC_API_KEY=...         # alternative
```

Lookup order: shell environment → `.env` in the project directory → `~/.media_manager/.env`. The startup banner shows which credential was found. Without credentials the scanner and UI still work; only AI endpoints are disabled.

### Optional: FireCrawl

For long-tail releases where WebSearch comes up short, you can set a FireCrawl API key in Settings → AI Integration. The AI will use it as a fallback; it's off by default and not required.

## Storage

Everything Musaic persists lives under `~/.musaic/` (override with `MUSAIC_CONFIG_DIR`):

| Path | Purpose |
|---|---|
| `config.json` | Selected library directory |
| `library.db` | SQLite cache of scanned album metadata, jobs, findings, settings |
| `thumbs/{album_id}.jpg` | Pre-extracted cover thumbnails |
| `.env` | Optional credentials |

The FLAC files and `cover.jpg` sidecars on disk remain the source of truth for audio content; the SQLite database is a cache and state store.

## Server flags

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | Bind address |
| `--port` | `8765` | Port |
| `--reload` | off | Hot-reload on code changes |
