//! The desktop shell's layer in front of musaic-api's router.
//!
//! Who may call the API is decided in musaic-api (the per-launch token
//! lives there: [`musaic_api::launch`]). The backend is
//! started with `serve_loopback(.., launch_token: true, ..)`, so the launch
//! URL, the cookie and the 403 all come from musaic-api; this layer only
//! asks [`LaunchGate::authorizes`] for its own endpoints. It adds what is
//! specific to the window:
//!
//! * **CSP** on every response that has none (musaic-api sets one only on
//!   the cover preview, a stricter `default-src 'none'; sandbox`): the
//!   webview may load and connect only to its own origin; see [`APP_CSP`].
//! * **Second-instance focus**: `POST /__musaic/focus` with the
//!   `X-Musaic-Focus` secret from the instance lock file (instance.rs). The
//!   second process has no cookie; the secret is its credential.
//! * **`window.confirm` / `window.alert` shim endpoint**:
//!   `POST /__musaic/dialog/{confirm,alert}`. wry has no WKUIDelegate
//!   confirm/alert panel on macOS, so `window.confirm()` returns `false`
//!   without asking and "Remove from library" / "Clear anchor" could never
//!   be confirmed. An initialization script (lib.rs, [`DIALOG_SHIM_JS`])
//!   replaces both with a synchronous same-origin XHR to this endpoint,
//!   which shows a native dialog and answers `true` / `false`. It needs the
//!   exact `127.0.0.1:<port>` Host, the launch cookie and the app's own
//!   Origin.
//! * **Native folder picker**: `POST
//!   /__musaic/dialog/folder` opens the system "choose a folder" panel
//!   (`tauri-plugin-dialog`, called from Rust) and answers
//!   `{"path": "/abs/dir"}` or `{"path": null}` when cancelled. Same
//!   checks as the confirm shim. The page learns that it runs in the
//!   window from the marker the initialization script defines
//!   (`window.__musaicDesktop.folderPicker`); a plain browser has none and
//!   keeps the text field. The plugin's own IPC commands stay unreachable:
//!   the page is a remote URL to Tauri and no capability is granted.
//! * **Reveal in the file manager** ("Show in Finder"): `POST /__musaic/reveal/album/{album_id}` shows the album's
//!   folder, `POST /__musaic/reveal/trash` the app's trash folder
//!   (`<config>/trash`, created when absent), selected in a Finder /
//!   Explorer / file-manager window (`tauri-plugin-opener`'s
//!   `reveal_item_in_dir`, called from Rust). The page sends an album
//!   **id**, never a path: the folder is looked up here the way
//!   `GET /api/albums/{id}/tracks` finds it (musaic-api
//!   `ops().library().album_folder`: the configured library's cached row, else a
//!   live walk of the library), so the page
//!   can only reveal an album folder of the configured library or the
//!   trash. 204 when shown; 404 for an unknown id or a folder that is not on
//!   disk (a Missing album); 409 when no library is configured; 500 when
//!   the file manager could not be asked. Same checks as the folder
//!   picker (cookie, exact Host, the app's own Origin, POST). The marker
//!   advertises it (`reveal: true`, `fileManager: "Finder" | "Explorer" |
//!   "file manager"`).
//! * **Native menu state** (`POST /__musaic/menu-state`): the page reports
//!   which native menu commands are enabled and which check items are
//!   ticked (menu.rs, [`crate::menu::parse_state`]: allow-listed ids only).
//!   Same checks as the reveal endpoints. 204; 400 for a malformed body.
//!   The marker advertises a menu bar (`nativeMenu: true`) only where the
//!   shell has one (macOS).
//! * **Selftest controls** (`--selftest-report` runs only; 404 otherwise):
//!   `POST /__musaic/selftest/{cmd}` lets the hidden-window tests hold a
//!   piece of work and send a quit request down the same paths the close
//!   button and the app menu's Quit use (lib.rs, [`Hooks::selftest`]).
//!   Same checks as the reveal endpoints.
//!
//! Order, outermost first: this layer → musaic-api's Host / Origin check →
//! launch token → routes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use musaic_api::launch::ct_eq;
use musaic_api::{AppState, LaunchGate};
use tokio::sync::oneshot;

pub use musaic_api::launch::{COOKIE, LAUNCH_PATH, NO_TOKEN, random_token};
pub const FOCUS_PATH: &str = "/__musaic/focus";
pub const DIALOG_PREFIX: &str = "/__musaic/dialog/";
/// The folder picker endpoint (under [`DIALOG_PREFIX`]).
pub const FOLDER_PATH: &str = "/__musaic/dialog/folder";
pub const FOCUS_HEADER: &str = "x-musaic-focus";
/// Reveal endpoints.
pub const REVEAL_PREFIX: &str = "/__musaic/reveal/";
/// `POST /__musaic/reveal/album/{album_id}`.
pub const REVEAL_ALBUM_PREFIX: &str = "/__musaic/reveal/album/";
/// `POST /__musaic/reveal/trash`.
pub const REVEAL_TRASH_PATH: &str = "/__musaic/reveal/trash";
/// `POST /__musaic/menu-state` (menu.rs).
pub const MENU_STATE_PATH: &str = "/__musaic/menu-state";
/// Selftest controls (`--selftest-report` runs only).
pub const SELFTEST_PREFIX: &str = "/__musaic/selftest/";

/// What the platform's file manager is called, for the page's "Show in …"
/// label (the marker's `fileManager`).
pub const FILE_MANAGER: &str = if cfg!(target_os = "macos") {
    "Finder"
} else if cfg!(windows) {
    "Explorer"
} else {
    "file manager"
};

/// Whether the shell shows a native menu bar whose commands the page drives
/// (menu.rs): macOS only. Elsewhere every command stays in the window.
pub const NATIVE_MENU: bool = cfg!(target_os = "macos");

/// Content-Security-Policy on every backend response. Only the app's own
/// origin: no remote scripts, styles, fonts, images or connections.
/// `style-src 'unsafe-inline'` is needed for the React/Radix inline style
/// attributes; `data:` / `blob:` images for the cover-upload preview
/// (`URL.createObjectURL`). The font is bundled
/// (`frontend/public/fonts/`), and a proposed remote cover is shown through
/// the same-origin `GET /api/ai/jobs/{job_id}/cover-preview` or
/// `GET /api/albums/{album_id}/cover-preview` (musaic-api
/// `routes::preview`), so neither needs a remote origin here.
pub const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; media-src 'self'; worker-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

/// Injected into the main frame of every page (Tauri initialization
/// script, not subject to page CSP). Only acts on the backend origin.
/// The window uses [`init_script`], which fills in the file manager's name.
pub const DIALOG_SHIM_JS: &str = r#"(function () {
  if (location.protocol !== "http:" || location.hostname !== "127.0.0.1") return;
  function ask(kind, message) {
    try {
      var x = new XMLHttpRequest();
      x.open("POST", "/__musaic/dialog/" + kind, false);
      x.setRequestHeader("Content-Type", "text/plain;charset=UTF-8");
      x.send(message === undefined ? "" : String(message));
      return x.status === 200 && x.responseText === "true";
    } catch (e) {
      return false;
    }
  }
  window.confirm = function (message) { return ask("confirm", message); };
  window.alert = function (message) { ask("alert", message); };
  // Marker for the frontend (lib/desktop.ts): this window can show a
  // native folder picker through POST /__musaic/dialog/folder and reveal
  // an album folder / the trash through POST /__musaic/reveal/*. With
  // nativeMenu, the menu bar sends "musaic:command" events (menu.rs) and
  // takes its enabled state from POST /__musaic/menu-state.
  Object.defineProperty(window, "__musaicDesktop", {
    value: Object.freeze({ folderPicker: true, reveal: true, fileManager: "__MUSAIC_FILE_MANAGER__", nativeMenu: __MUSAIC_NATIVE_MENU__ }),
    enumerable: false, configurable: false, writable: false
  });
})();"#;

/// The initialization script the window runs: [`DIALOG_SHIM_JS`] with the
/// platform's [`FILE_MANAGER`] name and [`NATIVE_MENU`] in the marker.
pub fn init_script() -> String {
    DIALOG_SHIM_JS
        .replace("__MUSAIC_FILE_MANAGER__", FILE_MANAGER)
        .replace(
            "__MUSAIC_NATIVE_MENU__",
            if NATIVE_MENU { "true" } else { "false" },
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Confirm,
    Alert,
}

/// What the gate calls back into (the Tauri side, or a test double).
pub trait Hooks: Send + Sync {
    /// A second launch asked this instance to come to the front.
    fn focus(&self);
    /// Show a native dialog; the receiver gets the answer.
    fn dialog(&self, kind: DialogKind, message: String) -> oneshot::Receiver<bool>;
    /// Show the native "choose a folder" panel; the receiver gets the
    /// chosen directory, or `None` when cancelled. A dropped sender counts
    /// as cancelled. The default (no window) answers `None`.
    fn pick_folder(&self) -> oneshot::Receiver<Option<PathBuf>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(None);
        rx
    }
    /// Show `path` selected in the platform's file manager. The receiver
    /// gets `Err(reason)` when the file manager could not be asked. The
    /// default (no window) refuses.
    fn reveal(&self, _path: PathBuf) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err("no window".into()));
        rx
    }
    /// The page's native menu state (allow-listed items only). The default
    /// (no menu bar) ignores it.
    fn menu_state(&self, _items: Vec<crate::menu::ItemState>) {}
    /// An authenticated request reached the backend (selftest bookkeeping).
    fn seen(&self, _method: &Method, _path: &str) {}
    /// A selftest control (`POST /__musaic/selftest/{cmd}?{query}`).
    /// `None`: not a selftest run, answered 404 (the default). `Err` is a
    /// 400 with the reason.
    fn selftest(
        &self,
        _cmd: &str,
        _query: Option<&str>,
        _app: &AppState,
    ) -> Option<Result<serde_json::Value, String>> {
        None
    }
}

pub struct Gate {
    launch: Arc<LaunchGate>,
    focus_secret: String,
    hooks: Arc<dyn Hooks>,
    /// The served app: where an album id's folder and the trash are.
    app: AppState,
}

impl Gate {
    /// `launch` is musaic-api's gate for the bound port; `app` the state
    /// the router serves.
    pub fn new(
        launch: Arc<LaunchGate>,
        focus_secret: String,
        hooks: Arc<dyn Hooks>,
        app: AppState,
    ) -> Self {
        Gate {
            launch,
            focus_secret,
            hooks,
            app,
        }
    }

    pub fn origin(&self) -> &str {
        self.launch.origin()
    }

    /// The URL the window opens first.
    pub fn launch_url(&self) -> String {
        self.launch.launch_url()
    }
}

fn detail(status: StatusCode, msg: &str) -> Response {
    let body = serde_json::json!({ "detail": msg }).to_string();
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

fn with_csp(mut r: Response) -> Response {
    let h = r.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(APP_CSP),
        );
    }
    r
}

/// The middleware. Mount outside musaic-api's router:
/// `router.layer(axum::middleware::from_fn_with_state(gate, guard))`.
pub async fn guard(State(gate): State<Arc<Gate>>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // The agent route family has its own guard in musaic-api (loopback peer,
    // exact Host, no Origin, a bearer token) and never takes the launch cookie:
    // pass it straight through, untouched.
    if path.starts_with(musaic_api::agent_guard::PREFIX) {
        return next.run(req).await;
    }
    if path == FOCUS_PATH {
        let ok = req.method() == Method::POST
            && gate.launch.host_is_self(req.headers())
            && req
                .headers()
                .get(FOCUS_HEADER)
                .is_some_and(|v| ct_eq(v.as_bytes(), gate.focus_secret.as_bytes()));
        if !ok {
            return detail(StatusCode::FORBIDDEN, "Forbidden");
        }
        gate.hooks.focus();
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NO_CONTENT;
        return r;
    }
    if let Some(kind) = path.strip_prefix(DIALOG_PREFIX) {
        let h = req.headers();
        if !gate.launch.host_is_self(h) || !gate.launch.authorizes(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, NO_TOKEN));
        }
        return with_csp(dialog(&gate, kind, req).await);
    }
    if let Some(what) = path.strip_prefix(REVEAL_PREFIX) {
        let h = req.headers();
        if !gate.launch.host_is_self(h) || !gate.launch.authorizes(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, NO_TOKEN));
        }
        if req.method() != Method::POST || !gate.launch.origin_is_self(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, "Forbidden"));
        }
        return with_csp(reveal(&gate, what).await);
    }
    if path == MENU_STATE_PATH {
        let h = req.headers();
        if !gate.launch.host_is_self(h) || !gate.launch.authorizes(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, NO_TOKEN));
        }
        if req.method() != Method::POST || !gate.launch.origin_is_self(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, "Forbidden"));
        }
        return with_csp(menu_state(&gate, req).await);
    }
    if let Some(cmd) = path.strip_prefix(SELFTEST_PREFIX) {
        let h = req.headers();
        if !gate.launch.host_is_self(h) || !gate.launch.authorizes(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, NO_TOKEN));
        }
        if req.method() != Method::POST || !gate.launch.origin_is_self(h) {
            return with_csp(detail(StatusCode::FORBIDDEN, "Forbidden"));
        }
        let r = match gate.hooks.selftest(cmd, req.uri().query(), &gate.app) {
            None => detail(StatusCode::NOT_FOUND, "Not Found"),
            Some(Err(e)) => detail(StatusCode::BAD_REQUEST, &e),
            Some(Ok(v)) => {
                let mut r = Response::new(Body::from(v.to_string()));
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                r
            }
        };
        return with_csp(r);
    }
    if gate.launch.authorizes(req.headers()) {
        gate.hooks.seen(req.method(), &path);
    }
    with_csp(next.run(req).await)
}

const DIALOG_BODY_LIMIT: usize = 16 * 1024;

async fn menu_state(gate: &Gate, req: Request) -> Response {
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), crate::menu::STATE_BODY_LIMIT).await
    else {
        return detail(StatusCode::PAYLOAD_TOO_LARGE, "Menu state too large");
    };
    match crate::menu::parse_state(&bytes) {
        Ok(items) => {
            gate.hooks.menu_state(items);
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::NO_CONTENT;
            r
        }
        Err(e) => detail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn dialog(gate: &Gate, kind: &str, req: Request) -> Response {
    let kind = match kind {
        "confirm" => DialogKind::Confirm,
        "alert" => DialogKind::Alert,
        "folder" => {
            if req.method() != Method::POST || !gate.launch.origin_is_self(req.headers()) {
                return detail(StatusCode::FORBIDDEN, "Forbidden");
            }
            return pick_folder(gate).await;
        }
        _ => return detail(StatusCode::NOT_FOUND, "Not Found"),
    };
    if req.method() != Method::POST || !gate.launch.origin_is_self(req.headers()) {
        return detail(StatusCode::FORBIDDEN, "Forbidden");
    }
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), DIALOG_BODY_LIMIT).await else {
        return detail(StatusCode::PAYLOAD_TOO_LARGE, "Message too long");
    };
    let msg = String::from_utf8_lossy(&bytes).into_owned();
    let answer = gate.hooks.dialog(kind, msg).await.unwrap_or(false);
    let mut r = Response::new(Body::from(if answer { "true" } else { "false" }));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

/// `{"path": "<dir>"}`, `{"path": null}` when cancelled, or a 422 when the
/// chosen path is not valid UTF-8 (the library path is stored as text).
async fn pick_folder(gate: &Gate) -> Response {
    let chosen = gate.hooks.pick_folder().await.unwrap_or(None);
    let path = match chosen {
        None => serde_json::Value::Null,
        Some(p) => match p.into_os_string().into_string() {
            Ok(s) => serde_json::Value::String(s),
            Err(_) => {
                return detail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "The chosen folder's path can't be stored as text. Choose another folder.",
                );
            }
        },
    };
    let mut r = Response::new(Body::from(serde_json::json!({ "path": path }).to_string()));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

/// An album id as `musaic_core::album_id` makes it: 16 lower-case hex
/// characters. Anything else is refused before a lookup.
fn is_album_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Why no folder is revealed: the status and `detail` message to answer
/// with (kept small; the response is built in [`reveal`]).
type Refusal = (StatusCode, String);

fn refusal(status: StatusCode, msg: &str) -> Refusal {
    (status, msg.to_string())
}

/// The album's folder, looked up as `GET /api/albums/{id}/tracks` does; why
/// not when there is none.
fn album_folder(app: &AppState, album_id: &str) -> Result<PathBuf, Refusal> {
    let path = app.ops().library().album_folder(album_id).map_err(|e| {
        let e = musaic_api::http::ApiError::from(e);
        match StatusCode::from_u16(e.status) {
            Ok(status) if status != StatusCode::INTERNAL_SERVER_ERROR => {
                refusal(status, e.detail.as_str().unwrap_or("Album not found"))
            }
            _ => refusal(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        }
    })?;
    if !path.is_dir() {
        return Err(refusal(
            StatusCode::NOT_FOUND,
            "This album's folder isn't on disk right now.",
        ));
    }
    Ok(path)
}

/// The app trash (`<config>/trash`), created when nothing was trashed yet so
/// there is something to show.
fn trash_folder(config_dir: &Path) -> Result<PathBuf, Refusal> {
    let t = config_dir.join("trash");
    std::fs::create_dir_all(&t).map_err(|_| {
        refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Couldn't create the trash folder.",
        )
    })?;
    Ok(t)
}

async fn reveal(gate: &Gate, what: &str) -> Response {
    let target = if what == "trash" {
        let dir = gate.app.config_dir().to_path_buf();
        tokio::task::spawn_blocking(move || trash_folder(&dir)).await
    } else if let Some(id) = what.strip_prefix("album/")
        && is_album_id(id)
    {
        let (app, id) = (gate.app.clone(), id.to_string());
        tokio::task::spawn_blocking(move || album_folder(&app, &id)).await
    } else {
        return detail(StatusCode::NOT_FOUND, "Not Found");
    };
    let path = match target {
        Ok(Ok(p)) => p,
        Ok(Err((status, msg))) => return detail(status, &msg),
        Err(_) => return detail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    };
    match gate.hooks.reveal(path).await {
        Ok(Ok(())) => {
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::NO_CONTENT;
            r
        }
        _ => detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Couldn't open the file manager.",
        ),
    }
}
