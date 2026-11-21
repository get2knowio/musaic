//! The native menu bar (macOS) and how its commands reach the page.
//!
//! The menu is described here as data ([`MENU_BAR`], [`COMMANDS`]); lib.rs
//! turns it into Tauri menus on macOS. Windows and Linux have no menu bar:
//! every command has an in-window equivalent there (the frontend's command
//! registry, `frontend/src/lib/commands.ts`, drives both).
//!
//! The webview has no IPC, so a command travels without one:
//!
//! * **Shell → page.** A click on a command item (or its key equivalent)
//!   evaluates [`dispatch_script`] in the main window: a constant
//!   `window.dispatchEvent(new CustomEvent("musaic:command", {detail: "<id>"}))`
//!   whose id is the `&'static str` of an entry in [`COMMANDS`], never text
//!   from the event or the page. The page's command host
//!   (`frontend/src/hooks/useCommands.ts`) runs the same registry command
//!   its buttons and shortcuts run, after checking that it is enabled. The
//!   script is evaluated only while the window shows the backend origin.
//!   Script the shell evaluates is not subject to the page's CSP, so the
//!   CSP stays as it is; the event carries nothing the page could not
//!   already do itself.
//! * **Page → shell.** Which commands are enabled (and which check items
//!   are ticked) is the page's knowledge: the selection, a busy scan, an
//!   open modal. The page posts it to `POST /__musaic/menu-state` (gate.rs;
//!   cookie, exact Host, own Origin, like the reveal endpoints), and
//!   [`parse_state`] keeps only ids in [`COMMANDS`]. Until the page has
//!   posted, every command item is dimmed.

/// The DOM event the page listens for.
pub const EVENT: &str = "musaic:command";

/// How a command shows in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A plain item.
    Normal,
    /// An item with a check mark (Tiles / List, a sort order, a theme).
    Check,
}

/// One command item. Its `id` is also the frontend registry's command id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    /// Tauri accelerator text.
    pub accel: Option<&'static str>,
    pub kind: Kind,
    /// The command asks for more input before it does anything (a dialog,
    /// a form, a picker): its label ends with an ellipsis.
    pub asks: bool,
}

const fn cmd(id: &'static str, label: &'static str, accel: Option<&'static str>) -> Command {
    Command {
        id,
        label,
        accel,
        kind: Kind::Normal,
        asks: false,
    }
}

const fn asks(id: &'static str, label: &'static str, accel: Option<&'static str>) -> Command {
    Command {
        id,
        label,
        accel,
        kind: Kind::Normal,
        asks: true,
    }
}

const fn check(id: &'static str, label: &'static str) -> Command {
    Command {
        id,
        label,
        accel: None,
        kind: Kind::Check,
        asks: false,
    }
}

/// Every command the menu bar can send to the page: the allow-list.
pub const COMMANDS: &[Command] = &[
    asks("app.settings", "Settings…", Some("CmdOrCtrl+,")),
    asks("library.choose-folder", "Choose Music Folder…", None),
    cmd("library.rescan", "Rescan Library", None),
    cmd("library.cancel-scan", "Cancel Scan", None),
    asks("catalog.backup", "Back Up Catalog Database…", None),
    asks("catalog.restore", "Restore Catalog Database…", None),
    cmd("library.find", "Find in Library", Some("CmdOrCtrl+F")),
    cmd("view.library", "Show Library", Some("CmdOrCtrl+1")),
    cmd("view.review", "Show Review", Some("CmdOrCtrl+2")),
    cmd("view.activity", "Show Activity", None),
    check("view.tiles", "Tiles"),
    check("view.list", "List"),
    check("sort.artist-asc", "Artist · A → Z"),
    check("sort.artist-desc", "Artist · Z → A"),
    check("sort.title-asc", "Title · A → Z"),
    check("sort.title-desc", "Title · Z → A"),
    check("sort.year-desc", "Year · newest"),
    check("sort.year-asc", "Year · oldest"),
    check("sort.scanned-desc", "Recently scanned"),
    check("sort.scanned-asc", "Least recently scanned"),
    check("appearance.light", "Light"),
    check("appearance.dark", "Dark"),
    cmd("album.reveal", "Show in Finder", None),
    asks("album.my-tags", "Edit My Tags…", None),
    asks("album.audio-analysis", "Audio Analysis…", None),
    cmd("help.shortcuts", "Keyboard Shortcuts", None),
];

/// The allow-listed command with this id.
pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// Standard items the platform implements itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Std {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    CloseWindow,
    Minimize,
    Zoom,
    Fullscreen,
    BringAllToFront,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Command(&'static str),
    Std(Std),
    /// Musaic's own Quit item (quit.rs: the predefined one would quit
    /// without asking while work runs).
    Quit,
    Separator,
    Submenu(&'static str, &'static [Entry]),
}

/// Which special role a top-level menu has for the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    None,
    /// The application menu, titled with the app's name.
    App,
    /// macOS adds the window list here.
    Window,
    /// macOS adds the Help search field here.
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Top {
    pub title: &'static str,
    pub role: Role,
    pub entries: &'static [Entry],
}

use Entry::{Command as C, Separator as Sep, Std as S};

const SORT_BY: &[Entry] = &[
    C("sort.artist-asc"),
    C("sort.artist-desc"),
    C("sort.title-asc"),
    C("sort.title-desc"),
    C("sort.year-desc"),
    C("sort.year-asc"),
    C("sort.scanned-desc"),
    C("sort.scanned-asc"),
];

const APPEARANCE: &[Entry] = &[C("appearance.light"), C("appearance.dark")];

/// The menu bar, in display order (#334).
pub const MENU_BAR: &[Top] = &[
    Top {
        title: "Musaic",
        role: Role::App,
        entries: &[
            S(Std::About),
            Sep,
            C("app.settings"),
            Sep,
            S(Std::Services),
            Sep,
            S(Std::Hide),
            S(Std::HideOthers),
            S(Std::ShowAll),
            Sep,
            Entry::Quit,
        ],
    },
    Top {
        title: "File",
        role: Role::None,
        entries: &[
            C("library.choose-folder"),
            Sep,
            C("library.rescan"),
            C("library.cancel-scan"),
            Sep,
            C("catalog.backup"),
            C("catalog.restore"),
            Sep,
            S(Std::CloseWindow),
        ],
    },
    Top {
        title: "Edit",
        role: Role::None,
        entries: &[
            S(Std::Undo),
            S(Std::Redo),
            Sep,
            S(Std::Cut),
            S(Std::Copy),
            S(Std::Paste),
            S(Std::SelectAll),
            Sep,
            C("library.find"),
        ],
    },
    Top {
        title: "View",
        role: Role::None,
        entries: &[
            C("view.library"),
            C("view.review"),
            C("view.activity"),
            Sep,
            C("view.tiles"),
            C("view.list"),
            Entry::Submenu("Sort By", SORT_BY),
            Sep,
            Entry::Submenu("Appearance", APPEARANCE),
            Sep,
            S(Std::Fullscreen),
        ],
    },
    Top {
        title: "Album",
        role: Role::None,
        entries: &[
            C("album.reveal"),
            Sep,
            C("album.my-tags"),
            C("album.audio-analysis"),
        ],
    },
    Top {
        title: "Window",
        role: Role::Window,
        entries: &[S(Std::Minimize), S(Std::Zoom), Sep, S(Std::BringAllToFront)],
    },
    Top {
        title: "Help",
        role: Role::Help,
        entries: &[C("help.shortcuts")],
    },
];

/// The script that hands `cmd` to the page. Built only from the
/// allow-listed id, so nothing from outside the table reaches the webview.
pub fn dispatch_script(cmd: &'static Command) -> String {
    format!(
        "window.dispatchEvent(new CustomEvent({EVENT:?}, {{ detail: {:?} }}));",
        cmd.id
    )
}

/// What the page says about one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemState {
    pub command: &'static Command,
    pub enabled: bool,
    /// For check items: ticked or not; ignored for plain items.
    pub checked: Option<bool>,
}

/// The largest menu-state body the shell reads.
pub const STATE_BODY_LIMIT: usize = 16 * 1024;

/// Parse the page's `{"items": [{"id", "enabled", "checked"?}, …]}`. Ids
/// not in [`COMMANDS`] are dropped; a malformed body is an error.
pub fn parse_state(body: &[u8]) -> Result<Vec<ItemState>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
    let items = v
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or("expected {\"items\": [...]}")?;
    let mut out = Vec::new();
    for it in items {
        let Some(id) = it.get("id").and_then(serde_json::Value::as_str) else {
            return Err("each item needs an \"id\"".into());
        };
        let Some(enabled) = it.get("enabled").and_then(serde_json::Value::as_bool) else {
            return Err(format!("{id}: \"enabled\" must be true or false"));
        };
        let checked = it.get("checked").and_then(serde_json::Value::as_bool);
        if let Some(command) = command(id) {
            out.push(ItemState {
                command,
                enabled,
                checked: if command.kind == Kind::Check {
                    checked
                } else {
                    None
                },
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(entries: &'static [Entry], out: &mut Vec<&'static str>) {
        for e in entries {
            match e {
                Entry::Command(id) => out.push(id),
                Entry::Submenu(_, inner) => walk(inner, out),
                _ => {}
            }
        }
    }

    fn placed() -> Vec<&'static str> {
        let mut out = Vec::new();
        for top in MENU_BAR {
            walk(top.entries, &mut out);
        }
        out
    }

    #[test]
    fn menu_bar_follows_the_334_hierarchy() {
        let titles: Vec<_> = MENU_BAR.iter().map(|t| t.title).collect();
        assert_eq!(
            titles,
            ["Musaic", "File", "Edit", "View", "Album", "Window", "Help"]
        );
        assert_eq!(MENU_BAR[0].role, Role::App);
        assert_eq!(MENU_BAR[5].role, Role::Window);
        assert_eq!(MENU_BAR[6].role, Role::Help);
        // The app menu keeps About, Hide and our own Quit (busy-quit).
        let app = MENU_BAR[0].entries;
        assert_eq!(app.first(), Some(&Entry::Std(Std::About)));
        assert_eq!(app.last(), Some(&Entry::Quit));
        assert!(app.contains(&Entry::Command("app.settings")));
        assert!(app.contains(&Entry::Std(Std::Hide)));
        // Exactly one Quit, in the app menu.
        let quits: usize = MENU_BAR
            .iter()
            .map(|t| t.entries.iter().filter(|e| **e == Entry::Quit).count())
            .sum();
        assert_eq!(quits, 1);
        // Native text editing stays.
        let edit = MENU_BAR[2].entries;
        for s in [
            Std::Undo,
            Std::Redo,
            Std::Cut,
            Std::Copy,
            Std::Paste,
            Std::SelectAll,
        ] {
            assert!(edit.contains(&Entry::Std(s)), "{s:?}");
        }
        assert!(MENU_BAR[1].entries.contains(&Entry::Std(Std::CloseWindow)));
        // First-release commands are in their menus.
        for (menu, id) in [
            (0, "app.settings"),
            (1, "library.rescan"),
            (2, "library.find"),
            (3, "view.library"),
            (3, "view.review"),
            (4, "album.reveal"),
            (6, "help.shortcuts"),
        ] {
            let mut ids = Vec::new();
            walk(MENU_BAR[menu].entries, &mut ids);
            assert!(ids.contains(&id), "{id} in {}", MENU_BAR[menu].title);
        }
    }

    #[test]
    fn every_command_is_placed_once_and_every_placed_id_is_allow_listed() {
        let placed = placed();
        for c in COMMANDS {
            assert_eq!(
                placed.iter().filter(|p| **p == c.id).count(),
                1,
                "{} placed once",
                c.id
            );
        }
        for p in &placed {
            assert!(command(p).is_some(), "{p} not in COMMANDS");
        }
    }

    #[test]
    fn first_release_shortcuts() {
        let accel = |id| command(id).unwrap().accel;
        assert_eq!(accel("app.settings"), Some("CmdOrCtrl+,"));
        assert_eq!(accel("library.find"), Some("CmdOrCtrl+F"));
        assert_eq!(accel("view.library"), Some("CmdOrCtrl+1"));
        assert_eq!(accel("view.review"), Some("CmdOrCtrl+2"));
        let mut seen = std::collections::HashSet::new();
        for c in COMMANDS {
            if let Some(a) = c.accel {
                assert!(seen.insert(a), "{a} used twice");
                // Cmd-Q and Cmd-W belong to Quit and Close Window.
                assert!(!matches!(a, "CmdOrCtrl+Q" | "CmdOrCtrl+W"), "{a}");
            }
        }
    }

    #[test]
    fn ellipsis_marks_commands_that_ask_for_input() {
        for c in COMMANDS {
            assert_eq!(c.label.ends_with('…'), c.asks, "{}", c.label);
            assert!(!c.label.ends_with("..."), "{}: use …", c.label);
        }
    }

    #[test]
    fn ids_are_plain_tokens() {
        for c in COMMANDS {
            assert!(
                !c.id.is_empty()
                    && c.id
                        .bytes()
                        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-')),
                "{}",
                c.id
            );
            assert_ne!(c.id, crate::quit::QUIT_MENU_ID);
        }
    }

    #[test]
    fn dispatch_script_is_a_constant_event_for_an_allow_listed_id() {
        let c = command("view.review").unwrap();
        assert_eq!(
            dispatch_script(c),
            r#"window.dispatchEvent(new CustomEvent("musaic:command", { detail: "view.review" }));"#
        );
        for c in COMMANDS {
            let s = dispatch_script(c);
            assert!(s.starts_with("window.dispatchEvent(new CustomEvent(\"musaic:command\""));
            assert_eq!(s.matches(c.id).count(), 1);
        }
        assert!(command("view.review\"));alert(1);//").is_none());
        assert!(command("musaic-quit").is_none());
    }

    #[test]
    fn parse_state_keeps_known_ids_only() {
        let body = br#"{"items": [
            {"id": "album.reveal", "enabled": false},
            {"id": "view.tiles", "enabled": true, "checked": true},
            {"id": "library.rescan", "enabled": true, "checked": true},
            {"id": "evil.command", "enabled": true}
        ]}"#;
        let s = parse_state(body).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].command.id, "album.reveal");
        assert!(!s[0].enabled);
        assert_eq!(s[1].checked, Some(true));
        // A plain item has no check mark to set.
        assert_eq!(s[2].checked, None);
        assert!(parse_state(b"nope").is_err());
        assert!(parse_state(br#"{"items": [{"id": "album.reveal"}]}"#).is_err());
        assert!(parse_state(br#"{"other": []}"#).is_err());
    }

    /// The frontend registry names every native command, so a menu item
    /// always has something to run.
    #[test]
    fn frontend_registry_names_every_command() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../frontend/src/lib/commands.ts"
        );
        let src = std::fs::read_to_string(path).expect("frontend/src/lib/commands.ts");
        for c in COMMANDS {
            assert!(
                src.contains(&format!("\"{}\"", c.id)),
                "{} missing from commands.ts",
                c.id
            );
        }
        assert!(src.contains(EVENT), "commands.ts names the {EVENT} event");
    }
}
