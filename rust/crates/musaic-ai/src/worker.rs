//! The AI job worker (`musaic/services/worker.py`): one job
//! ([`run_one_job`]) and the dispatcher loop
//! ([`Worker`]).
//!
//! Per job: mark it running, resolve the album first, then synthesize the
//! metadata suggestion from the resolution when [`can_skip_provider`] says
//! so (no provider call), else build the Python app's request and call the
//! [`Provider`]; same for the cover half with [`cover_from_resolver`]. A
//! provider MBID is verified against MusicBrainz and stored as the anchor
//! at score 88 (`AlbumMbAnchorSet("ai")`); no MBID at all records a lookup
//! failure. A completed, identified, change-free result is auto-acknowledged
//! when its MBID was stored or came from the resolver.
//! Any failure marks the job failed (with the transcript excerpt) and pauses
//! the queue; the 240 s wall-clock cap gives the timeout message.
//! A job that fails only because it needs a model and the provider has no backend
//! ([`Provider::has_backend`], i.e. [`crate::Unconfigured`]) fails with the
//! not-available message but does not pause the queue.
//!
//! The loop: startup recovery of `running` rows, then dispatch the oldest
//! queued job while not paused and fewer than `ai_max_parallel` run, each
//! job on its own thread, waking on [`Signal`] or every 30 s.
//!
//! Deliberate addition (restore gate): a job runs while
//! holding the gate's work guard ([`WorkerEnv::begin_work`]), so a database
//! restore is refused while a job writes, and no job is dispatched while a
//! restore runs (the loop retries every second). The Python app has no gate.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use musaic_core::AiWording;
use musaic_db::ai_jobs::{self, JobRow};
use musaic_resolve::AlbumResolution;
use rusqlite::Connection;

use crate::jobs::{Signal, max_parallel, queue_paused, set_queue_paused};
use crate::parse::{CoverSuggestion, MetadataSuggestion, parse_cover, parse_metadata};
use crate::payload::{album_payload, format_user_message};
use crate::prompts::{
    AGENT_MAX_TURNS, FIRECRAWL_MCP_URL_TEMPLATE, FIRECRAWL_PROMPT_HINT, SYSTEM_PROMPT_COVER,
    SYSTEM_PROMPT_METADATA,
};
use crate::provider::{
    CallKind, ImagePart, McpServer, Provider, ProviderError, ProviderEvent, ProviderRequest,
    model_unavailable,
};
use crate::pyjson::{Py, py_strip};
use crate::synth::{can_skip_provider, cover_from_resolver, synthesize_cover, synthesize_metadata};
use crate::transcript::{Buffer, EXCERPT_MAX_BYTES, Recorder, format_transcript};

/// `_AI_CALL_WALL_CLOCK_TIMEOUT_S`.
pub const WALL_CLOCK_TIMEOUT: Duration = Duration::from_secs(240);
/// `_AI_MBID_CONFIDENCE` (`mb_lookup.HIGH_CONFIDENCE_SCORE`).
pub const AI_MBID_SCORE: i64 = 88;
/// The dispatcher's idle wait (`asyncio.wait_for(jobs_changed.wait(), 30)`).
pub const IDLE_WAIT: Duration = Duration::from_secs(30);

/// The timeout failure's error text (the cap in whole minutes).
pub fn timeout_message() -> String {
    let mins = WALL_CLOCK_TIMEOUT.as_secs() / 60;
    format!(
        "AI call exceeded the {mins}-minute wall-clock cap and was cancelled. Almost always means \
         an upstream tool (MusicBrainz / Cover Art Archive / FireCrawl) is slow or down \u{2014} the \
         Claude subprocess sits idle waiting for a tool response that never returns. Hit Retry once \
         the affected service is responsive, or skip this album and come back to it later."
    )
}

/// The timeout's excerpt when nothing was recorded.
pub fn silent_timeout_excerpt() -> String {
    let mins = WALL_CLOCK_TIMEOUT.as_secs() / 60;
    format!(
        "(no SDK messages received in {mins} minutes \u{2014} the Claude subprocess was completely \
         silent until the wall-clock cap fired. Usually this means the subprocess was wedged before \
         the first model response: Anthropic API not reachable, FireCrawl MCP server failing to \
         connect, or the subprocess itself failed to initialize. Check ~/.musaic/server.log for any \
         related warnings around the job's start time.)"
    )
}

/// What the worker needs from the host (database, album data, resolver,
/// network checks, events, the restore gate).
pub trait WorkerEnv: Send + Sync {
    /// A database connection (`db.connect()`).
    fn open_db(&self) -> Result<Connection, String>;
    /// `_now_iso()`.
    fn now_iso(&self) -> String;
    /// `notify_jobs_changed()`.
    fn notify(&self);
    fn log(&self, line: &str);
    /// `services.albums.cached_album_dict(album_id)`.
    fn album(&self, conn: &Connection, album_id: &str) -> Option<Py>;
    /// `services.albums.read_tracks(album["path"])`.
    fn read_tracks(&self, album_path: &str) -> Py;
    /// `services.resolver.resolve_album(album_id, user_hint=hint)`.
    fn resolve(
        &self,
        conn: &Connection,
        album_id: &str,
        hint: &str,
    ) -> Result<Arc<AlbumResolution>, String>;
    /// `ai._album_cover_for_prompt(album)`.
    fn cover_for_prompt(&self, album: &Py) -> Option<ImagePart>;
    /// `settings.firecrawl_api_key()` (`None` or empty when unset).
    fn firecrawl_key(&self) -> Option<String>;
    /// `ai._verify_cover_url(url)`: (ok, reason).
    fn verify_cover_url(&self, url: &str) -> (bool, String);
    /// `mb_lookup.find_cover(album, hint=...)`: `{cover_url, source, notes,
    /// confidence}`, `Err` for an exception.
    fn find_cover(&self, conn: &Connection, album: &Py, hint: &str) -> Result<Py, String>;
    /// `persist_ai_mbid`'s MusicBrainz GET of the release-group: the status,
    /// `Err` for a request that did not complete.
    fn verify_release_group(&self, mbid: &str) -> Result<u16, String>;
    /// Publish `AlbumMbAnchorSet(album_id, source="ai")`.
    fn anchor_set(&self, album_id: &str);
    /// The restore gate's work guard; `None` while a restore runs.
    fn begin_work(&self) -> Option<Box<dyn Send>> {
        Some(Box::new(()))
    }
    /// `false` once the host has gone (the loop ends).
    fn alive(&self) -> bool {
        true
    }
}

/// Knobs (tests shorten them).
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// The cap on one provider call. The failure text always names
    /// [`WALL_CLOCK_TIMEOUT`].
    pub call_timeout: Duration,
    pub idle_wait: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            call_timeout: WALL_CLOCK_TIMEOUT,
            idle_wait: IDLE_WAIT,
        }
    }
}

/// Why a job failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The wall-clock cap.
    Timeout,
    Provider(ProviderError),
    /// Any other exception text.
    Other(String),
}

impl Failure {
    fn message(&self, provider_id: &str) -> String {
        match self {
            Failure::Timeout => timeout_message(),
            Failure::Provider(e) => e.message(provider_id),
            Failure::Other(m) => m.clone(),
        }
    }
}

fn db_err(e: impl std::fmt::Display) -> Failure {
    Failure::Other(e.to_string())
}

/// `_build_agent_extras(system_prompt)`.
fn agent_extras(env: &dyn WorkerEnv, system: &str) -> (String, Vec<McpServer>) {
    match env.firecrawl_key().filter(|k| !k.is_empty()) {
        None => (system.to_string(), Vec::new()),
        Some(key) => (
            format!("{system}{FIRECRAWL_PROMPT_HINT}"),
            vec![McpServer {
                name: "firecrawl".into(),
                transport: "sse".into(),
                url: FIRECRAWL_MCP_URL_TEMPLATE.replace("{api_key}", &key),
            }],
        ),
    }
}

/// `_run_agent` + `_agent_async`: the call on its own thread, its events
/// into the job's transcript, the final text (the last non-empty
/// `TurnComplete`, else the text deltas joined). `Err(None)` is the
/// wall-clock cap; the call's thread is abandoned (the Python app abandons its
/// worker thread too).
fn call_provider(
    provider: &Arc<dyn Provider>,
    request: ProviderRequest,
    buffer: &Buffer,
    timeout: Duration,
) -> Result<String, Option<ProviderError>> {
    let (tx, rx) = mpsc::channel();
    let p = provider.clone();
    let buf = buffer.clone();
    let spawned = std::thread::Builder::new()
        .name("ai-provider-call".into())
        .spawn(move || {
            let mut rec = Recorder::new(buf);
            let mut text_acc = String::new();
            let mut final_text = String::new();
            let r = p.run(&request, &mut |e: ProviderEvent| {
                rec.feed(&e);
                match &e {
                    ProviderEvent::Text(t) => text_acc.push_str(t),
                    ProviderEvent::TurnComplete { text } if !text.is_empty() => {
                        final_text = text.clone();
                    }
                    _ => {}
                }
            });
            rec.finish();
            let out = r.map(|()| {
                if final_text.is_empty() {
                    text_acc
                } else {
                    final_text
                }
            });
            let _ = tx.send(out);
        });
    if let Err(e) = spawned {
        return Err(Some(ProviderError::Other {
            message: format!("cannot start the provider call: {e}"),
            cause: None,
        }));
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r.map_err(Some),
        Err(RecvTimeoutError::Timeout) => Err(None),
        Err(RecvTimeoutError::Disconnected) => Err(Some(ProviderError::Other {
            message: "the provider call ended without an answer".into(),
            cause: None,
        })),
    }
}

struct Call<'a> {
    env: &'a dyn WorkerEnv,
    provider: &'a Arc<dyn Provider>,
    conn: &'a Connection,
    buffer: &'a Buffer,
    opts: &'a RunOptions,
}

impl Call<'_> {
    /// The wording of the text this job stores, from the
    /// provider's readiness when the job runs.
    fn wording(&self) -> AiWording {
        AiWording::for_readiness(self.provider.info().ready)
    }

    /// `ai.suggest_metadata(album, tracks, hint, resolution, attach_cover)`.
    fn suggest_metadata(
        &self,
        album: &Py,
        tracks: &Py,
        hint: &str,
        resolution: &AlbumResolution,
        attach_cover: bool,
    ) -> Result<MetadataSuggestion, Failure> {
        let payload = album_payload(album, tracks, Some(resolution));
        let user_message = format_user_message(&payload, hint);
        let (system_prompt, mcp_servers) = agent_extras(self.env, SYSTEM_PROMPT_METADATA);
        let image = if attach_cover {
            self.env.cover_for_prompt(album)
        } else {
            None
        };
        let request = ProviderRequest {
            kind: CallKind::Metadata,
            system_prompt,
            user_message,
            payload,
            image,
            mcp_servers,
            max_turns: AGENT_MAX_TURNS,
        };
        let raw = call_provider(self.provider, request, self.buffer, self.opts.call_timeout)
            .map_err(|e| e.map_or(Failure::Timeout, Failure::Provider))?;
        parse_metadata(&raw).map_err(Failure::Other)
    }

    /// `ai.suggest_cover(album, tracks, hint, resolution)` with its
    /// MusicBrainz fallbacks. The wall-clock cap is not caught by the
    /// fallback (in the Python app it fires outside `suggest_cover`).
    fn suggest_cover(
        &self,
        album: &Py,
        tracks: &Py,
        hint: &str,
        resolution: &AlbumResolution,
    ) -> Result<CoverSuggestion, Failure> {
        let payload = album_payload(album, tracks, Some(resolution));
        let user_message = format_user_message(&payload, hint);
        let (system_prompt, mcp_servers) = agent_extras(self.env, SYSTEM_PROMPT_COVER);
        let request = ProviderRequest {
            kind: CallKind::Cover,
            system_prompt,
            user_message,
            payload,
            image: None,
            mcp_servers,
            max_turns: AGENT_MAX_TURNS,
        };
        let raw = match call_provider(self.provider, request, self.buffer, self.opts.call_timeout) {
            Ok(r) => r,
            Err(None) => return Err(Failure::Timeout),
            Err(Some(e)) => return self.fallback_cover(album, hint, Some(Failure::Provider(e))),
        };
        let mut c = parse_cover(&raw);
        if let Some(url) = c.cover_url.clone() {
            let (ok, reason) = self.env.verify_cover_url(&url);
            if !ok {
                let verify_note =
                    format!("Claude's suggested URL didn't resolve to an image ({reason}).");
                c.notes = if c.notes.is_empty() {
                    verify_note
                } else {
                    py_strip(&format!("{}\n\n{verify_note}", c.notes)).to_string()
                };
                c.cover_url = None;
                let mb = self.fallback_cover(album, hint, None)?;
                if mb.cover_url.is_some() {
                    return Ok(CoverSuggestion {
                        cover_url: mb.cover_url,
                        source: mb.source,
                        notes: py_strip(&format!(
                            "{}\n\nFell back to MusicBrainz: {}",
                            c.notes, mb.notes
                        ))
                        .to_string(),
                        confidence: mb.confidence,
                    });
                }
                if !mb.notes.is_empty() {
                    c.notes = py_strip(&format!("{}\n\n{}", c.notes, mb.notes)).to_string();
                }
            }
        }
        Ok(c)
    }

    /// `ai._fallback_cover_lookup(album, hint, ai_error=...)`.
    fn fallback_cover(
        &self,
        album: &Py,
        hint: &str,
        ai_error: Option<Failure>,
    ) -> Result<CoverSuggestion, Failure> {
        let found = match self.env.find_cover(self.conn, album, hint) {
            Ok(d) => d,
            Err(e) => {
                self.env.log(&format!("MB fallback also failed: {e}"));
                return Err(ai_error.unwrap_or(Failure::Other(e)));
            }
        };
        let field = |k: &str, default: &str| match found.get(k) {
            Some(v) if v.truthy() => v.py_str(),
            _ => default.to_string(),
        };
        let cover_url = found
            .get("cover_url")
            .filter(|v| v.truthy())
            .map(Py::py_str);
        let mut notes = field("notes", "");
        if ai_error.is_some() {
            // Reached with no provider ready too (the provider
            // refuses, the MusicBrainz fallback runs).
            let prefix = self
                .wording()
                .pick(FALLBACK_COVER_PREFIX, FALLBACK_COVER_PREFIX_NEUTRAL);
            notes = if notes.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}\n\n{notes}")
            };
        }
        Ok(CoverSuggestion {
            cover_url,
            source: field("source", ""),
            notes,
            confidence: field("confidence", "low"),
        })
    }

    /// `worker.persist_ai_mbid(album_id, mbid)`: refused when blocklisted;
    /// verified with a MusicBrainz GET (a request that does not complete
    /// changes nothing; a non-200 records a lookup failure); then stored
    /// at score 88 and `AlbumMbAnchorSet("ai")` published. `Ok(true)` only
    /// when the anchor was stored.
    fn persist_ai_mbid(&self, album_id: &str, mbid: &str) -> Result<bool, String> {
        let blocked = musaic_db::mb_anchor::get_blocked_mb_release_groups(self.conn, album_id)
            .map_err(|e| e.to_string())?;
        if blocked.contains(mbid) {
            self.env.log(&format!(
                "AI returned blocklisted MBID for album={album_id} mbid={mbid}; refusing to persist"
            ));
            return Ok(false);
        }
        match self.env.verify_release_group(mbid) {
            Err(e) => {
                self.env.log(&format!(
                    "AI MBID verify failed for album={album_id} mbid={mbid}: {e}"
                ));
                Ok(false)
            }
            Ok(200) => {
                musaic_db::mb_anchor::set_album_mb_anchor(
                    self.conn,
                    album_id,
                    mbid,
                    None,
                    AI_MBID_SCORE,
                    &self.env.now_iso(),
                )
                .map_err(|e| e.to_string())?;
                self.env.anchor_set(album_id);
                Ok(true)
            }
            Ok(status) => {
                self.env.log(&format!(
                    "AI returned non-existent MBID for album={album_id} mbid={mbid} \
                     (MB returned HTTP {status}); ignoring"
                ));
                if let Err(e) =
                    ai_jobs::record_mb_lookup_failure(self.conn, album_id, &self.env.now_iso())
                {
                    self.env.log(&format!(
                        "Couldn't record MB lookup failure for album={album_id}: {e}"
                    ));
                }
                Ok(false)
            }
        }
    }
}

/// `worker._is_no_op_confirmation(result)`: identified (a non-blank
/// MBID) and nothing to apply (no album field, no track field, no cover URL).
pub fn is_no_op_confirmation(result: &Py) -> bool {
    let empty = Py::Dict(Vec::new());
    let meta = result
        .get("metadata")
        .filter(|m| m.truthy())
        .unwrap_or(&empty);
    let cover = result.get("cover").filter(|c| c.truthy()).unwrap_or(&empty);
    let mbid = match meta.get("mb_release_group_mbid") {
        Some(v) if v.truthy() => py_strip(&v.py_str()).to_string(),
        _ => String::new(),
    };
    if mbid.is_empty() {
        return false;
    }
    if meta.get("suggestions").is_some_and(Py::truthy) {
        return false;
    }
    if let Some(Py::List(tracks)) = meta.get("tracks")
        && tracks
            .iter()
            .any(|t| t.get("suggestions").is_some_and(Py::truthy))
    {
        return false;
    }
    !cover.get("cover_url").is_some_and(Py::truthy)
}

/// The Python app's prefix of the cover notes when the provider call failed and
/// the MusicBrainz fallback ran (`ai._fallback_cover_lookup`).
pub const FALLBACK_COVER_PREFIX: &str = "Claude's cover-art search exited with an error before returning a result; tried MusicBrainz directly as a fallback.";

/// [`FALLBACK_COVER_PREFIX`] when
/// no AI provider is ready. Without a backend the provider refuses at once
/// and the fallback is the whole lookup.
pub const FALLBACK_COVER_PREFIX_NEUTRAL: &str = "Looked up the cover on MusicBrainz directly.";

fn work(call: &Call<'_>, job: &JobRow) -> Result<(), Failure> {
    let env = call.env;
    let conn = call.conn;
    let album = env.album(conn, &job.album_id).ok_or_else(|| {
        Failure::Other(format!(
            "Album {} no longer in cache",
            musaic_fsops::py_repr(&job.album_id)
        ))
    })?;
    let path = album.get("path").map(Py::py_str).unwrap_or_default();
    let tracks = env.read_tracks(&path);
    let resolution = env
        .resolve(conn, &job.album_id, &job.hint)
        .map_err(Failure::Other)?;

    let mut result = Py::Dict(Vec::new());
    // Whether the result's MBID was
    // stored as the anchor or came from the resolver; only then is a no-op
    // result auto-acknowledged. The Python app acknowledged it whenever the
    // provider named an MBID, even one that 404'd, failed to verify or was
    // blocklisted (nothing stored).
    let mut mbid_trusted = false;
    if job.job_type == "metadata" || job.job_type == "metadata+cover" {
        let skip = can_skip_provider(&resolution);
        let meta = if skip {
            env.log(&format!(
                "job {} ({}, album={}): deterministic resolver covered everything; AI skipped",
                job.id, job.job_type, job.album_id
            ));
            synthesize_metadata(&album, &tracks, &resolution, call.wording())
        } else {
            call.suggest_metadata(&album, &tracks, &job.hint, &resolution, job.attach_cover)?
        };
        result.set("metadata", meta.to_py());
        match &meta.mb_release_group_mbid {
            Some(mbid) if !skip => {
                let from_resolver = resolution
                    .anchor
                    .as_ref()
                    .is_some_and(|a| py_strip(&a.value) == py_strip(mbid));
                match call.persist_ai_mbid(&job.album_id, mbid) {
                    Ok(stored) => mbid_trusted = stored || from_resolver,
                    Err(e) => {
                        mbid_trusted = from_resolver;
                        env.log(&format!(
                            "AI MBID persist failed for job {} (album={}, mbid={mbid}): {e}",
                            job.id, job.album_id
                        ));
                    }
                }
            }
            // The synthesizer's MBID is the resolver's anchor.
            Some(_) => mbid_trusted = true,
            None => {
                if let Err(e) =
                    ai_jobs::record_mb_lookup_failure(conn, &job.album_id, &env.now_iso())
                {
                    env.log(&format!(
                        "Couldn't record MB lookup failure for album={}: {e}",
                        job.album_id
                    ));
                }
            }
        }
    }
    if job.job_type == "cover" || job.job_type == "metadata+cover" {
        let cover = if cover_from_resolver(&resolution) {
            synthesize_cover(&resolution, call.wording())
        } else {
            call.suggest_cover(&album, &tracks, &job.hint, &resolution)?
        };
        result.set("cover", cover.to_py());
    }
    ai_jobs::mark_completed(conn, &job.id, &result.dumps(), &env.now_iso()).map_err(db_err)?;
    if mbid_trusted && is_no_op_confirmation(&result) {
        ai_jobs::mark_acted_on(conn, &job.id, "applied", &env.now_iso()).map_err(db_err)?;
    }
    Ok(())
}

/// The failure is the provider's "cannot run" answer and the
/// provider has no backend at all ([`Provider::has_backend`]). Any other
/// failure, and every failure of a configured backend, pauses the queue.
fn needs_absent_model(provider: &dyn Provider, failure: &Failure) -> bool {
    !provider.has_backend() && matches!(failure, Failure::Provider(ProviderError::NotReady { .. }))
}

/// `worker.run_one_job(job)`.
pub fn run_one_job(
    env: &dyn WorkerEnv,
    provider: &Arc<dyn Provider>,
    job: &JobRow,
    opts: &RunOptions,
) {
    let conn = match env.open_db() {
        Ok(c) => c,
        Err(e) => {
            env.log(&format!("job {}: cannot open the database: {e}", job.id));
            return;
        }
    };
    if let Err(e) = ai_jobs::mark_running(&conn, &job.id, &env.now_iso()) {
        env.log(&format!("job {}: mark_running failed: {e}", job.id));
    }
    env.notify();
    let buffer = Buffer::new();
    let call = Call {
        env,
        provider,
        conn: &conn,
        buffer: &buffer,
        opts,
    };
    let outcome = work(&call, job);
    if let Err(failure) = outcome {
        // A job that needed a model where no backend
        // exists fails with the not-available message ([`model_unavailable`]).
        let msg = match model_unavailable(provider.as_ref()) {
            Some(m) if needs_absent_model(provider.as_ref(), &failure) => m,
            _ => failure.message(provider.id()),
        };
        let mut excerpt = format_transcript(&buffer.lines(), EXCERPT_MAX_BYTES);
        if failure == Failure::Timeout && excerpt.is_none() {
            excerpt = Some(silent_timeout_excerpt());
        }
        let result_json = excerpt.map(|x| Py::dict([("failure_excerpt", Py::str(x))]).dumps());
        if let Err(e) =
            ai_jobs::mark_failed(&conn, &job.id, &msg, result_json.as_deref(), &env.now_iso())
        {
            env.log(&format!("job {}: mark_failed failed: {e}", job.id));
        }
        if needs_absent_model(provider.as_ref(), &failure) {
            // The job needed a model and this
            // build has none; the job fails with the not-available message
            // and the jobs behind it keep running. The Python app paused.
            env.log(&format!(
                "job {}: needs a model and no AI backend is configured; queue not paused",
                job.id
            ));
        } else {
            match set_queue_paused(&conn, true) {
                Ok(()) => env.log("AI queue paused due to job failure; user must resume manually"),
                Err(e) => env.log(&format!(
                    "Failed to set ai_queue_paused after job failure: {e}"
                )),
            }
        }
        if failure == Failure::Timeout {
            env.notify();
        }
    }
    env.notify();
}

/// Run the oldest queued job now, on this thread (holding the work guard).
/// Returns its id; `None` when nothing is queued or a restore runs.
pub fn run_next(
    env: &dyn WorkerEnv,
    provider: &Arc<dyn Provider>,
    opts: &RunOptions,
) -> Option<String> {
    let conn = env.open_db().ok()?;
    let job = ai_jobs::next_queued_excluding(&conn, &[]).ok()??;
    drop(conn);
    let guard = env.begin_work()?;
    run_one_job(env, provider, &job, opts);
    drop(guard);
    Some(job.id)
}

/// The dispatcher loop's handle.
pub struct Worker {
    stop: Arc<AtomicBool>,
    signal: Arc<Signal>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    /// `worker_loop()` on its own thread: startup recovery, then dispatch.
    pub fn start(
        env: Arc<dyn WorkerEnv>,
        provider: Arc<dyn Provider>,
        signal: Arc<Signal>,
        opts: RunOptions,
    ) -> Worker {
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, sig2) = (stop.clone(), signal.clone());
        let handle = std::thread::Builder::new()
            .name("ai-worker".into())
            .spawn(move || dispatch_loop(env, provider, sig2, opts, s2))
            .ok();
        Worker {
            stop,
            signal,
            handle: Mutex::new(handle),
        }
    }

    /// Stop dispatching and wait for the loop thread (jobs in flight finish
    /// on their own threads; a job left `running` is requeued at the next
    /// start).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.signal.notify();
        let h = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.signal.notify();
    }
}

fn dispatch_loop(
    env: Arc<dyn WorkerEnv>,
    provider: Arc<dyn Provider>,
    signal: Arc<Signal>,
    opts: RunOptions,
    stop: Arc<AtomicBool>,
) {
    // Startup recovery: rows left `running` by an earlier process.
    match env.open_db() {
        Ok(conn) => match ai_jobs::reset_stuck_running(&conn) {
            Ok(n) if n > 0 => {
                env.log(&format!(
                    "Reset {n} stuck running job(s) to queued on worker startup"
                ));
                env.notify();
            }
            Ok(_) => {}
            Err(e) => env.log(&format!("Worker startup recovery failed (continuing): {e}")),
        },
        Err(e) => env.log(&format!("Worker startup recovery failed (continuing): {e}")),
    }
    let in_flight: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    while !stop.load(Ordering::SeqCst) && env.alive() {
        let seen = signal.value();
        let conn = match env.open_db() {
            Ok(c) => c,
            Err(e) => {
                env.log(&format!(
                    "Worker dispatcher iteration failed; sleeping 5s and continuing: {e}"
                ));
                signal.wait_since(seen, Duration::from_secs(5));
                continue;
            }
        };
        let step = (|| -> musaic_db::Result<Option<JobRow>> {
            if queue_paused(&conn)? {
                return Ok(None);
            }
            let max = max_parallel(&conn)?;
            let flying: Vec<String> = in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect();
            let running = ai_jobs::count_running(&conn)?.max(flying.len() as i64);
            if running >= max {
                return Ok(None);
            }
            ai_jobs::next_queued_excluding(&conn, &flying)
        })();
        drop(conn);
        let job = match step {
            Ok(Some(j)) => j,
            Ok(None) => {
                signal.wait_since(seen, opts.idle_wait);
                continue;
            }
            Err(e) => {
                env.log(&format!(
                    "Worker dispatcher iteration failed; sleeping 5s and continuing: {e}"
                ));
                signal.wait_since(seen, Duration::from_secs(5));
                continue;
            }
        };
        let Some(guard) = env.begin_work() else {
            // A restore holds the gate: try again shortly.
            signal.wait_since(seen, Duration::from_secs(1));
            continue;
        };
        in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job.id.clone());
        let (env2, p2, f2) = (env.clone(), provider.clone(), in_flight.clone());
        let id = job.id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ai-job-{}", job.id))
            .spawn(move || {
                let _guard = guard;
                run_one_job(env2.as_ref(), &p2, &job, &opts);
                f2.lock().unwrap_or_else(|e| e.into_inner()).remove(&job.id);
                // `_run_and_track`'s `finally: jobs_changed.set()`.
                env2.notify();
            });
        if let Err(e) = spawned {
            in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            env.log(&format!("cannot start a job thread: {e}"));
            signal.wait_since(seen, Duration::from_secs(5));
        }
    }
    env.log("AI job worker stopped");
}
