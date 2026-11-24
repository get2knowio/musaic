//! `musaic-desktop`: the Tauri 2 shell (BRIEF.md: "Tauri 2 + existing React").
//!
//! Model (the Electron shell's, minus the sidecar process): the musaic-api
//! router runs **in this process** on `127.0.0.1:<ephemeral port>`, serving
//! both `/api` and the Python app's built React SPA, and the one window loads
//! that origin. Relative `fetch('/api/…')`, `EventSource`, the `postSse`
//! fetch streams and `<img src="/api/albums/…/cover">` all work unchanged,
//! so the frontend transport adapter is zero lines. Rationale and
//! the rejected alternatives (Tauri custom protocol, IPC bridge) are in
//! docs/desktop.md.
//!
//! Start-up: options → single-instance lock on the config dir (instance.rs)
//! → Tauri app → window on the local boot page → background thread builds
//! `AppState`, binds, serves (backend.rs) → window navigates to the
//! one-shot launch URL, which sets the per-launch cookie (musaic-api's
//! launch token).
//!
//! Log: `<config>/server.log`, the server binary's rotating,
//! secret-masked file, started once the instance lock is held.
//!
//! Folder picker: `tauri-plugin-dialog`, reached
//! only through the gate's `POST /__musaic/dialog/folder`; no IPC.
//!
//! `tauri-plugin-opener`, called from Rust only, opens allowed
//! external links in the system browser ([`nav`]) and reveals an album
//! folder or the trash (`POST /__musaic/reveal/*`, [`gate`]); its link
//! click script is off. `tauri-plugin-window-state` remembers size and
//! position in `<config>/window-state.json` for shown runs only
//! ([`window_state`]). Single instance stays this crate's own lock
//! ([`instance`]); `tauri-plugin-single-instance` is not used.
//!
//! Quitting: the window's close button (and Window → Close),
//! the app menu's Quit and Cmd-Q ask first while work runs
//! ([`quit::QuitGate`]; the native dialog here is the only part a hidden
//! test cannot show). On macOS the menu bar is Musaic's own ([`menu`]),
//! whose app menu has our own Quit item ([`quit::QUIT_MENU_ID`], Cmd-Q)
//! instead of the predefined one (which sends `terminate:`, unseen by
//! tao). Its command items reach the page as an allow-listed DOM event
//! evaluated in the window, and the page reports their enabled state to
//! `POST /__musaic/menu-state`; still no IPC ([`menu`]). Dock → Quit, the quit
//! Apple Event and logout still send `terminate:` and quit without asking
//! (tao has no `applicationShouldTerminate:`; docs/desktop.md "Quitting").
//! Signals never ask.
//!
//! Teardown: window closed, Cmd-Q, `AppHandle::exit`, SIGTERM /
//! SIGINT / SIGHUP (Unix), Ctrl-C and, on macOS, `terminate:` (Dock →
//! Quit, the quit Apple Event, logout: tao's `applicationWillTerminate:`)
//! all end in `RunEvent::Exit`, which ends
//! every SSE stream, stops the server (port released), runs
//! `AppState::shutdown` (fingerprint pool stopped with a bounded wait, AI
//! worker stopped, the database's WAL checkpointed) and
//! removes the instance info file. The shell spawns no child processes.

pub mod backend;
pub mod gate;
pub mod instance;
pub mod menu;
pub mod nav;
pub mod options;
pub mod quit;
pub mod report;
pub mod window_state;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use musaic_api::logging::{Level, ProcessLog};
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

use crate::backend::Backend;
use crate::gate::{DialogKind, Hooks};
use crate::instance::{Acquired, InstanceLock};
use crate::nav::Decision;
use crate::options::Options;
use crate::quit::{QuitGate, QuitOutcome, QuitRoute, QuitSource};
use crate::report::Report;

pub const MAIN_WINDOW: &str = "main";

struct Shell {
    opts: Options,
    report: Report,
    lock: InstanceLock,
    origin: Arc<OnceLock<String>>,
    backend: Mutex<Option<Backend>>,
    log: ProcessLog,
    /// One quit prompt at a time; a confirmed quit is not asked
    /// again.
    quit: QuitGate,
    /// Selftest runs: work held through `POST /__musaic/selftest/hold-work`.
    held: Mutex<Vec<musaic_api::WorkGuard>>,
    /// Selftest runs: the answer the next quit prompt records (`true`:
    /// "Quit").
    selftest_quit_answer: AtomicBool,
    /// The menu bar's command items (macOS; empty elsewhere).
    menu: OnceLock<MenuHandles>,
}

struct TauriHooks {
    app: AppHandle<Wry>,
    report: Report,
    /// `--hidden`: a second launch does not bring the window up.
    hidden: bool,
    /// For the selftest controls (weak: the shell owns the backend, which
    /// owns these hooks).
    shell: Weak<Shell>,
}

/// Open an allowed external URL (nav.rs decided) in the system browser.
fn open_external(app: &AppHandle<Wry>, url: &url::Url) {
    if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
        eprintln!("[musaic-desktop] cannot open {url} in the browser: {e}");
    }
}

impl Hooks for TauriHooks {
    fn focus(&self) {
        self.report.event("focus-requested", serde_json::json!({}));
        if self.hidden {
            return;
        }
        if let Some(w) = self.app.get_webview_window(MAIN_WINDOW) {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    fn dialog(&self, kind: DialogKind, message: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        if self.report.enabled() {
            // Selftest runs never block on a modal: answer "Cancel".
            self.report.event(
                "dialog",
                serde_json::json!({"kind": format!("{kind:?}"), "message": message, "answer": false}),
            );
            let _ = tx.send(false);
            return rx;
        }
        let res = self.app.run_on_main_thread(move || {
            let d = rfd::MessageDialog::new()
                .set_title("Musaic")
                .set_description(message);
            let answer = match kind {
                DialogKind::Confirm => {
                    d.set_level(rfd::MessageLevel::Warning)
                        .set_buttons(rfd::MessageButtons::OkCancel)
                        .show()
                        == rfd::MessageDialogResult::Ok
                }
                DialogKind::Alert => {
                    d.set_level(rfd::MessageLevel::Info)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                    true
                }
            };
            let _ = tx.send(answer);
        });
        if res.is_err() {
            // The receiver sees a dropped sender: answered as "Cancel".
        }
        rx
    }

    fn pick_folder(&self) -> oneshot::Receiver<Option<PathBuf>> {
        let (tx, rx) = oneshot::channel();
        if self.report.enabled() {
            // Selftest runs never block on a modal: answer "Cancel".
            self.report
                .event("folder-picker", serde_json::json!({"answer": null}));
            let _ = tx.send(None);
            return rx;
        }
        let mut d = self
            .app
            .dialog()
            .file()
            .set_title("Choose your music library folder");
        if let Some(w) = self.app.get_webview_window(MAIN_WINDOW) {
            d = d.set_parent(&w);
        }
        // Non-blocking: the callback runs once the panel closes; a path
        // that cannot be turned into a local path counts as cancelled.
        d.pick_folder(move |p| {
            let _ = tx.send(p.and_then(|p| p.into_path().ok()));
        });
        rx
    }

    fn reveal(&self, path: PathBuf) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        if self.report.enabled() {
            // Selftest runs never open a file manager window.
            self.report
                .event("reveal", serde_json::json!({"path": path, "shown": false}));
            let _ = tx.send(Ok(()));
            return rx;
        }
        let app = self.app.clone();
        // The plugin's own command runs this off the main thread too; on
        // Linux it is a blocking D-Bus call.
        tauri::async_runtime::spawn_blocking(move || {
            let r = app
                .opener()
                .reveal_item_in_dir(&path)
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        rx
    }

    fn open_bundle(&self, path: &Path) -> Result<(), String> {
        if self.report.enabled() {
            // Selftest runs record the request and open nothing.
            self.report
                .event("connect-claude", serde_json::json!({"opened": false}));
            return Ok(());
        }
        // The operating system hands the file to whatever opens `.mcpb`
        // files. Nothing is started here and no program is named.
        self.app
            .opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            // A fixed sentence: the opener's error can name the file, and the
            // answer never carries the generated file's path.
            .map_err(|_| "the system could not open the extension file".to_string())
    }

    fn menu_state(&self, items: Vec<menu::ItemState>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if let Some(handles) = shell.menu.get() {
            apply_menu_state(handles, &items);
        }
        if self.report.enabled() {
            let enabled: Vec<_> = items
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.command.id)
                .collect();
            self.report.event(
                "menu-state",
                serde_json::json!({"items": items.len(), "enabled": enabled}),
            );
        }
    }

    fn seen(&self, method: &axum::http::Method, path: &str) {
        self.report.seen(method.as_str(), path);
    }

    /// Selftest controls for the hidden-window quit test (tests/boot.rs):
    ///
    /// * `hold-work`: hold an "applying changes" work guard (the quit
    ///   check sees work running); `release-work` drops every held guard.
    /// * `quit?via=menu|close&answer=quit|keep`: on the main thread, run
    ///   the app menu Quit item's handler (`menu`) or close the window the
    ///   way its close button does (`close`: `Window::close` raises the same
    ///   `CloseRequested`); a prompt records `answer` (default `quit`).
    /// * `menu?id=<command>`: on the main thread, run what choosing that
    ///   menu command runs (menu.rs: the allow-listed event to the page).
    fn selftest(
        &self,
        cmd: &str,
        query: Option<&str>,
        app: &musaic_api::AppState,
    ) -> Option<Result<serde_json::Value, String>> {
        if !self.report.enabled() {
            return None;
        }
        let shell = self.shell.upgrade()?;
        let param = |name: &str| {
            query.and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.into_owned())
            })
        };
        let r = match cmd {
            "hold-work" => match app.app().gate().begin(musaic_api::WorkKind::Apply) {
                Ok(g) => {
                    let mut held = shell.held.lock().unwrap_or_else(PoisonError::into_inner);
                    held.push(g);
                    Ok(serde_json::json!({"held": held.len()}))
                }
                Err(e) => Err(format!("cannot hold work: {}", e.detail())),
            },
            "release-work" => {
                shell
                    .held
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clear();
                Ok(serde_json::json!({"held": 0}))
            }
            "quit" => {
                let answer = match param("answer").as_deref() {
                    None | Some("quit") => true,
                    Some("keep") => false,
                    Some(a) => return Some(Err(format!("unknown answer {a:?}"))),
                };
                let via = param("via").unwrap_or_default();
                if via != "menu" && via != "close" {
                    return Some(Err(format!("unknown via {via:?} (menu|close)")));
                }
                shell.selftest_quit_answer.store(answer, Ordering::SeqCst);
                let handle = self.app.clone();
                let s = shell.clone();
                let menu = via == "menu";
                let sent = self.app.run_on_main_thread(move || {
                    if menu {
                        quit_from_menu(&handle, &s);
                    } else if let Some(w) = handle.get_webview_window(MAIN_WINDOW) {
                        let _ = w.close();
                    }
                });
                sent.map(|()| serde_json::json!({"via": via, "answer": answer}))
                    .map_err(|e| e.to_string())
            }
            "menu" => {
                let id = param("id").unwrap_or_default();
                let Some(cmd) = menu::command(&id) else {
                    return Some(Err(format!("not a menu command: {id:?}")));
                };
                let handle = self.app.clone();
                let s = shell.clone();
                self.app
                    .run_on_main_thread(move || deliver_command(&handle, &s, cmd))
                    .map(|()| serde_json::json!({"id": cmd.id}))
                    .map_err(|e| e.to_string())
            }
            _ => return None,
        };
        Some(r)
    }
}

/// After the window-state plugin restored the saved geometry,
/// put the window back in the middle of the main display when too little
/// of its title bar is on any attached display.
fn keep_on_screen(w: &tauri::WebviewWindow<Wry>) {
    let (Ok(pos), Ok(size), Ok(monitors)) =
        (w.outer_position(), w.outer_size(), w.available_monitors())
    else {
        return;
    };
    let rect = window_state::Rect {
        x: pos.x.into(),
        y: pos.y.into(),
        w: size.width.into(),
        h: size.height.into(),
    };
    let screens: Vec<_> = monitors
        .iter()
        .map(|m| window_state::Rect {
            x: m.position().x.into(),
            y: m.position().y.into(),
            w: m.size().width.into(),
            h: m.size().height.into(),
        })
        .collect();
    if window_state::needs_recentring(rect, &screens) {
        let _ = w.center();
    }
}

/// The built SPA: `--frontend-dir` / `MUSAIC_FRONTEND_DIR`, else the
/// bundle's `frontend/` resource, else (debug builds only) the repo's
/// `frontend/dist`.
fn default_frontend(app: &AppHandle<Wry>) -> Option<PathBuf> {
    if let Ok(r) = app.path().resource_dir() {
        let d = r.join("frontend");
        if d.join("index.html").is_file() {
            return Some(d);
        }
    }
    if cfg!(debug_assertions) {
        let d = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../frontend/dist"
        ));
        if d.join("index.html").is_file() {
            return std::fs::canonicalize(&d).ok();
        }
    }
    None
}

fn boot(app: AppHandle<Wry>, shell: Arc<Shell>) {
    let fail = |msg: String| {
        eprintln!("[musaic-desktop] {msg}");
        shell
            .log
            .log
            .log_file_only(Level::Error, backend::TARGET, &msg);
        shell
            .report
            .event("boot-failed", serde_json::json!({"error": msg}));
        if let Some(w) = app.get_webview_window(MAIN_WINDOW)
            && let Ok(mut u) = w.url()
        {
            u.set_fragment(Some(&format!("error={msg}")));
            let _ = w.navigate(u);
            let _ = w.show();
        }
    };
    let mut opts = shell.opts.clone();
    if opts.frontend_dir.is_none() {
        opts.frontend_dir = default_frontend(&app);
    }
    let Some(frontend) = opts.frontend_dir.clone() else {
        fail("no built frontend found (pass --frontend-dir or MUSAIC_FRONTEND_DIR)".into());
        return;
    };
    let state = match backend::build_state(&opts, Some(&shell.log)) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let focus_secret = match gate::random_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let hooks = Arc::new(TauriHooks {
        app: app.clone(),
        report: shell.report.clone(),
        hidden: shell.opts.hidden,
        shell: Arc::downgrade(&shell),
    });
    let started = tauri::async_runtime::block_on(backend::start(
        state,
        opts.port,
        focus_secret.clone(),
        hooks,
    ));
    let backend = match started {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    if let Err(e) = shell.lock.publish(backend.addr.port(), &focus_secret) {
        eprintln!("[musaic-desktop] cannot write instance info: {e}");
    }
    let origin = backend.origin();
    let _ = shell.origin.set(origin.clone());
    let launch = backend.gate.launch_url();
    eprintln!("[musaic-desktop] serving {origin}");
    shell
        .log
        .log
        .log(Level::Info, backend::TARGET, &format!("serving {origin}"));
    shell.report.event(
        "backend",
        serde_json::json!({
            "origin": origin,
            "port": backend.addr.port(),
            "launch_url": launch,
            "frontend_dir": frontend,
            "config_dir": opts.config_dir,
            "pid": std::process::id(),
        }),
    );
    *shell.backend.lock().unwrap_or_else(PoisonError::into_inner) = Some(backend);
    match (
        app.get_webview_window(MAIN_WINDOW),
        url::Url::parse(&launch),
    ) {
        (Some(w), Ok(u)) => {
            if let Err(e) = w.navigate(u) {
                fail(format!("cannot open the window on the server: {e}"));
            }
        }
        _ => fail("main window is gone".into()),
    }
}

fn teardown(shell: &Shell) {
    // Selftest work guards end before the server stops.
    shell
        .held
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    let taken = shell
        .backend
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(b) = taken {
        let port = b.addr.port();
        let r = tauri::async_runtime::block_on(b.stop());
        shell.report.event(
            "stopped",
            serde_json::json!({"port": port, "drained": r.drained, "state_shutdown": r.state_shutdown}),
        );
    }
    shell.lock.retire();
    shell.report.event("exit", serde_json::json!({}));
    shell.log.log.flush(Duration::from_secs(2));
}

/// Whether a user's quit (or close) may go ahead now. When work
/// runs, asks (native dialog; a selftest run records the prompt and gives
/// the answer set through the selftest controls, "Quit" by default) and
/// returns the answer.
fn user_may_quit(shell: &Shell, route: QuitRoute) -> bool {
    let outcome = shell.quit.request(
        QuitSource::User,
        || {
            shell
                .backend
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(Backend::work)
        },
        |message| {
            if shell.report.enabled() {
                let answer = shell.selftest_quit_answer.load(Ordering::SeqCst);
                shell.report.event(
                    "quit-prompt",
                    serde_json::json!({"route": route.as_str(), "message": message, "answer": answer}),
                );
                return answer;
            }
            rfd::MessageDialog::new()
                .set_title(musaic_api::work::QUIT_TITLE)
                .set_description(message)
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::OkCancelCustom(
                    "Quit".into(),
                    "Keep running".into(),
                ))
                .show()
                == rfd::MessageDialogResult::Custom("Quit".into())
        },
    );
    shell.report.event(
        "quit-request",
        serde_json::json!({"route": route.as_str(), "outcome": outcome.as_str()}),
    );
    if matches!(
        outcome,
        QuitOutcome::Confirmed(_) | QuitOutcome::Cancelled(_)
    ) {
        shell.log.log.log(
            Level::Info,
            backend::TARGET,
            &format!("quit while busy ({}): {}", route.as_str(), outcome.as_str()),
        );
    }
    outcome.proceeds()
}

/// The app menu's Quit item / Cmd-Q: ask when work runs, then
/// exit through `AppHandle::exit` (whose exit request is ours and is not
/// asked again).
fn quit_from_menu(app: &AppHandle<Wry>, shell: &Shell) {
    if user_may_quit(shell, QuitRoute::QuitMenu) {
        app.exit(0);
    }
}

/// A command item of the menu bar, kept so the page's menu state can dim
/// or tick it.
#[derive(Clone)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum MenuHandle {
    Item(tauri::menu::MenuItem<Wry>),
    Check(tauri::menu::CheckMenuItem<Wry>),
}

type MenuHandles = std::collections::HashMap<&'static str, MenuHandle>;

/// macOS: the menu bar of menu.rs ([`menu::MENU_BAR`], #334). Command items
/// start dimmed until the page reports what is available. The app menu's
/// Quit is our own item (Cmd-Q, [`quit::QUIT_MENU_ID`]): the predefined
/// one sends `terminate:` straight to NSApp, so no event would reach us
/// and work in progress would not be asked about.
#[cfg(target_os = "macos")]
fn app_menu(
    app: &AppHandle<Wry>,
    handles: &OnceLock<MenuHandles>,
) -> tauri::Result<tauri::menu::Menu<Wry>> {
    use menu::{Entry, Role, Std};
    use tauri::menu::{
        AboutMetadata, CheckMenuItem, HELP_SUBMENU_ID, IsMenuItem, Menu, MenuItem,
        PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
    };

    fn entries(
        app: &AppHandle<Wry>,
        list: &'static [Entry],
        handles: &mut MenuHandles,
    ) -> tauri::Result<Vec<Box<dyn IsMenuItem<Wry>>>> {
        let mut out: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        for e in list {
            match *e {
                Entry::Separator => out.push(Box::new(PredefinedMenuItem::separator(app)?)),
                Entry::Quit => out.push(Box::new(MenuItem::with_id(
                    app,
                    quit::QUIT_MENU_ID,
                    format!("Quit {}", app.package_info().name),
                    true,
                    Some("CmdOrCtrl+Q"),
                )?)),
                Entry::Std(s) => out.push(Box::new(match s {
                    Std::About => {
                        let info = app.package_info();
                        let meta = AboutMetadata {
                            name: Some(info.name.clone()),
                            version: Some(info.version.to_string()),
                            copyright: app.config().bundle.copyright.clone(),
                            ..Default::default()
                        };
                        PredefinedMenuItem::about(app, None, Some(meta))?
                    }
                    Std::Services => PredefinedMenuItem::services(app, None)?,
                    Std::Hide => PredefinedMenuItem::hide(app, None)?,
                    Std::HideOthers => PredefinedMenuItem::hide_others(app, None)?,
                    Std::ShowAll => PredefinedMenuItem::show_all(app, None)?,
                    Std::Undo => PredefinedMenuItem::undo(app, None)?,
                    Std::Redo => PredefinedMenuItem::redo(app, None)?,
                    Std::Cut => PredefinedMenuItem::cut(app, None)?,
                    Std::Copy => PredefinedMenuItem::copy(app, None)?,
                    Std::Paste => PredefinedMenuItem::paste(app, None)?,
                    Std::SelectAll => PredefinedMenuItem::select_all(app, None)?,
                    Std::CloseWindow => PredefinedMenuItem::close_window(app, None)?,
                    Std::Minimize => PredefinedMenuItem::minimize(app, None)?,
                    Std::Zoom => PredefinedMenuItem::maximize(app, Some("Zoom"))?,
                    Std::Fullscreen => PredefinedMenuItem::fullscreen(app, None)?,
                    Std::BringAllToFront => PredefinedMenuItem::bring_all_to_front(app, None)?,
                })),
                Entry::Command(id) => {
                    let Some(c) = menu::command(id) else { continue };
                    match c.kind {
                        menu::Kind::Normal => {
                            let item = MenuItem::with_id(app, c.id, c.label, false, c.accel)?;
                            handles.insert(c.id, MenuHandle::Item(item.clone()));
                            out.push(Box::new(item));
                        }
                        menu::Kind::Check => {
                            let item =
                                CheckMenuItem::with_id(app, c.id, c.label, false, false, c.accel)?;
                            handles.insert(c.id, MenuHandle::Check(item.clone()));
                            out.push(Box::new(item));
                        }
                    }
                }
                Entry::Submenu(title, inner) => {
                    let items = entries(app, inner, handles)?;
                    let refs: Vec<&dyn IsMenuItem<Wry>> =
                        items.iter().map(|b| b.as_ref()).collect();
                    out.push(Box::new(Submenu::with_items(app, title, true, &refs)?));
                }
            }
        }
        Ok(out)
    }

    let mut map = MenuHandles::new();
    let bar = Menu::new(app)?;
    for top in menu::MENU_BAR {
        let items = entries(app, top.entries, &mut map)?;
        let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|b| b.as_ref()).collect();
        let sub = match top.role {
            Role::App => Submenu::with_items(app, app.package_info().name.clone(), true, &refs)?,
            Role::Window => {
                Submenu::with_id_and_items(app, WINDOW_SUBMENU_ID, top.title, true, &refs)?
            }
            Role::Help => Submenu::with_id_and_items(app, HELP_SUBMENU_ID, top.title, true, &refs)?,
            Role::None => Submenu::with_items(app, top.title, true, &refs)?,
        };
        bar.append(&sub)?;
    }
    let _ = handles.set(map);
    Ok(bar)
}

/// Apply the page's menu state to the command items.
fn apply_menu_state(handles: &MenuHandles, items: &[menu::ItemState]) {
    for s in items {
        match handles.get(s.command.id) {
            Some(MenuHandle::Item(i)) => {
                let _ = i.set_enabled(s.enabled);
            }
            Some(MenuHandle::Check(i)) => {
                let _ = i.set_enabled(s.enabled);
                if let Some(c) = s.checked {
                    let _ = i.set_checked(c);
                }
            }
            None => {}
        }
    }
}

/// A command item was chosen: hand its allow-listed id to the page
/// (menu.rs), only while the window shows the backend origin.
fn deliver_command(app: &AppHandle<Wry>, shell: &Shell, cmd: &'static menu::Command) {
    let delivered = match (app.get_webview_window(MAIN_WINDOW), shell.origin.get()) {
        (Some(w), Some(origin)) => {
            let on_backend = w
                .url()
                .is_ok_and(|u| u.origin().ascii_serialization() == *origin);
            on_backend && w.eval(menu::dispatch_script(cmd)).is_ok()
        }
        _ => false,
    };
    shell.report.event(
        "menu-command",
        serde_json::json!({"id": cmd.id, "delivered": delivered}),
    );
}

/// The menu bar as the selftest report shows it: each submenu's title and
/// its items (`kind`, `id`, `text`).
fn describe_menu(app: &AppHandle<Wry>) -> serde_json::Value {
    use tauri::menu::MenuItemKind;
    fn item(i: &MenuItemKind<Wry>) -> serde_json::Value {
        let (kind, text) = match i {
            MenuItemKind::MenuItem(m) => ("item", m.text().ok()),
            MenuItemKind::Submenu(m) => ("submenu", m.text().ok()),
            MenuItemKind::Predefined(m) => ("predefined", m.text().ok()),
            MenuItemKind::Check(m) => ("check", m.text().ok()),
            MenuItemKind::Icon(m) => ("icon", m.text().ok()),
        };
        serde_json::json!({"kind": kind, "id": i.id().0, "text": text})
    }
    let Some(menu) = app.menu() else {
        return serde_json::Value::Null;
    };
    let subs: Vec<_> = menu
        .items()
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.as_submenu().cloned())
        .map(|sub| {
            serde_json::json!({
                "title": sub.text().ok(),
                "items": sub.items().unwrap_or_default().iter().map(item).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::Value::Array(subs)
}

fn spawn_signal_watch(app: AppHandle<Wry>) {
    tauri::async_runtime::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
                _ = hup.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        app.exit(0);
    });
}

/// The process entry point; returns the exit code.
pub fn run() -> i32 {
    let opts = match Options::parse(&options::Process) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[musaic-desktop] {e}");
            return 2;
        }
    };
    let report = Report::new(opts.selftest_report.clone());
    let log_opts = opts.clone();
    let lock = match instance::acquire(&opts.config_dir) {
        Ok(Acquired::First(l)) => l,
        Ok(Acquired::Running) => {
            // A second launch focuses the first and exits.
            return match instance::ask_running_to_focus(&opts.config_dir, Duration::from_secs(15)) {
                Ok(()) => {
                    eprintln!("[musaic-desktop] already running; focused the existing window");
                    0
                }
                Err(e) => {
                    eprintln!("[musaic-desktop] already running but not answering: {e}");
                    1
                }
            };
        }
        Err(e) => {
            eprintln!(
                "[musaic-desktop] cannot lock {}: {e}",
                opts.config_dir.display()
            );
            return 1;
        }
    };
    // The server binary's log, once this process owns the dir.
    let log = backend::start_log(&log_opts);
    if let Err(e) = &log.opened {
        eprintln!("[musaic-desktop] Warning: couldn't set up file log ({e})");
    }
    let origin: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
    let shell = Arc::new(Shell {
        opts,
        report,
        lock,
        origin: origin.clone(),
        backend: Mutex::new(None),
        log,
        quit: QuitGate::new(),
        held: Mutex::new(Vec::new()),
        selftest_quit_answer: AtomicBool::new(true),
        menu: OnceLock::new(),
    });

    let setup_shell = shell.clone();
    let state_file = window_state::state_file(&shell.opts);
    let mut builder = tauri::Builder::default()
        // The native folder picker behind POST /__musaic/dialog/folder
        // (gate.rs). Called from Rust only: `app.security.capabilities` is
        // empty, so the page cannot invoke the plugin's commands.
        .plugin(tauri_plugin_dialog::init())
        // External links (nav.rs) and reveal (gate.rs), called
        // from Rust only. The plugin's link-click script is off: it would
        // `invoke` the plugin's IPC command, which the page may not call.
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        );
    // Window size and position, shown runs only (window_state.rs).
    if let Some(f) = state_file.as_ref().and_then(|p| p.to_str()) {
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(window_state::FLAGS)
                .with_filename(f)
                .build(),
        );
    }
    let remember_window = state_file.is_some();
    #[cfg(target_os = "macos")]
    {
        let s = shell.clone();
        builder = builder.menu(move |app| app_menu(app, &s.menu));
    }
    let menu_shell = shell.clone();
    builder = builder.on_menu_event(move |app, event| {
        let id = event.id().as_ref();
        if id == quit::QUIT_MENU_ID {
            quit_from_menu(app, &menu_shell);
        } else if let Some(cmd) = menu::command(id) {
            deliver_command(app, &menu_shell, cmd);
        }
    });
    let app = builder
        .setup(move |app| {
            let shell = setup_shell;
            let (o_nav, o_win, o_dl) = (origin.clone(), origin.clone(), origin.clone());
            let (h_nav, h_win) = (app.handle().clone(), app.handle().clone());
            let report = shell.report.clone();
            let window =
                WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
                    .title("Musaic")
                    .inner_size(1280.0, 860.0)
                    .min_inner_size(720.0, 480.0)
                    .visible(!shell.opts.hidden)
                    // HTML5 drag-and-drop must reach the page (CoverUploadDialog);
                    // Tauri's own file-drop handler would swallow it.
                    .disable_drag_drop_handler()
                    .initialization_script(gate::init_script())
                    .on_navigation(move |url| {
                        match nav::decide(url, o_nav.get().map(String::as_str)) {
                            Decision::Allow => true,
                            Decision::OpenExternally => {
                                open_external(&h_nav, url);
                                false
                            }
                            Decision::Deny => false,
                        }
                    })
                    .on_new_window(move |url, _features| {
                        if let Some(u) =
                            nav::new_window_target(&url, o_win.get().map(String::as_str))
                        {
                            open_external(&h_win, u);
                        }
                        NewWindowResponse::Deny
                    })
                    .on_download(move |_webview, event| match event {
                        DownloadEvent::Requested { url, .. } => {
                            nav::download_allowed(url.as_str(), o_dl.get().map(String::as_str))
                        }
                        _ => true,
                    })
                    .on_page_load(move |_w, p| {
                        let phase = match p.event() {
                            PageLoadEvent::Started => "started",
                            PageLoadEvent::Finished => "finished",
                        };
                        report.event(
                            "page-load",
                            serde_json::json!({"phase": phase, "url": p.url().as_str()}),
                        );
                    })
                    .build()?;
            if remember_window {
                keep_on_screen(&window);
            }
            shell.report.event(
                "window-created",
                serde_json::json!({
                    "label": window.label(),
                    "visible": !shell.opts.hidden,
                    "window_state": remember_window,
                }),
            );
            if shell.report.enabled() {
                shell.report.event(
                    "menu",
                    serde_json::json!({"menu": describe_menu(app.handle())}),
                );
            }
            spawn_signal_watch(app.handle().clone());
            let handle = app.handle().clone();
            let boot_shell = shell.clone();
            std::thread::Builder::new()
                .name("musaic-boot".into())
                .spawn(move || boot(handle, boot_shell))?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[musaic-desktop] cannot start the window system: {e}");
            shell.lock.retire();
            return 1;
        }
    };
    let exit_shell = shell.clone();
    app.run_return(move |_handle, event| match event {
        // The window's close button, Window → Close (Cmd-W).
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW => {
            if !user_may_quit(&exit_shell, QuitRoute::CloseButton) {
                api.prevent_close();
            }
        }
        // The last window gone (`code: None`); our own `exit`
        // (signals, the Quit item after it decided) carries a code and
        // never asks. (Cmd-Q is the Quit menu item: `on_menu_event`.)
        RunEvent::ExitRequested { code, api, .. } => {
            if quit::exit_request_source(code) == QuitSource::User
                && !user_may_quit(&exit_shell, QuitRoute::LastWindow)
            {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => teardown(&exit_shell),
        _ => {}
    })
}
