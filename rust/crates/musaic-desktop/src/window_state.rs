//! Window size and position remembered between launches
//! (`tauri-plugin-window-state`).
//!
//! * **Where**: `<config>/window-state.json`, next to `library.db`, so a
//!   `--config-dir` run keeps its own (the plugin joins its file name onto
//!   Tauri's `app_config_dir()`; an absolute name replaces that dir).
//! * **What**: size, position and maximized ([`FLAGS`]). Not visibility
//!   (restoring it would show a `--hidden` window), decorations or full
//!   screen.
//! * **When not**: `--hidden` and `--selftest-report` runs do not register
//!   the plugin at all, so they neither read nor write the file
//!   ([`state_file`]). Nor does a config dir whose path is not UTF-8 (the
//!   plugin takes the file name as a `String`).
//! * **Off-screen**: the plugin restores a saved position only when one of
//!   the window's corners is on an attached display, so a window saved on
//!   a display that is gone opens where the OS puts it. A corner can still
//!   leave nearly all of the window, title bar included, off-screen;
//!   [`needs_recentring`] checks after the restore that enough of the top
//!   strip (where the title bar is) is on a display, and the shell centres
//!   the window when it is not.

use std::path::PathBuf;

use tauri_plugin_window_state::StateFlags;

use crate::options::Options;

/// The state file's name in the config dir.
pub const FILE_NAME: &str = "window-state.json";

/// What is saved and restored.
pub const FLAGS: StateFlags = StateFlags::SIZE
    .union(StateFlags::POSITION)
    .union(StateFlags::MAXIMIZED);

/// The state file for this run, or `None` when the run must neither read
/// nor write window state (hidden or selftest runs, a non-UTF-8 config dir).
pub fn state_file(opts: &Options) -> Option<PathBuf> {
    if opts.hidden || opts.selftest_report.is_some() {
        return None;
    }
    let p = opts.config_dir.join(FILE_NAME);
    p.is_absolute()
        .then_some(p)
        .filter(|p| p.to_str().is_some())
}

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    fn overlap(&self, o: &Rect) -> (i64, i64) {
        let w = (self.x + self.w).min(o.x + o.w) - self.x.max(o.x);
        let h = (self.y + self.h).min(o.y + o.h) - self.y.max(o.y);
        (w.max(0), h.max(0))
    }
}

/// Height of the strip along the window's top edge that must be reachable
/// (the title bar, by which the window is dragged back), and how much of
/// its width must be on one display.
const GRAB_HEIGHT: i64 = 32;
const GRAB_WIDTH: i64 = 120;

/// Whether the window at `window` (outer frame, physical pixels) should be
/// moved back on screen: true unless some display (`monitors`, physical
/// pixels) shows at least [`GRAB_WIDTH`] × [`GRAB_HEIGHT`] of its top
/// strip (the whole strip for a narrower window). With no display
/// reported, leaves the window alone.
pub fn needs_recentring(window: Rect, monitors: &[Rect]) -> bool {
    if monitors.is_empty() {
        return false;
    }
    let strip = Rect {
        h: window.h.clamp(1, GRAB_HEIGHT),
        ..window
    };
    let need_w = strip.w.clamp(1, GRAB_WIDTH);
    !monitors.iter().any(|m| {
        let (w, h) = strip.overlap(m);
        w >= need_w && h >= strip.h
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{NetworkMode, Options};

    fn opts(hidden: bool, selftest: bool) -> Options {
        Options {
            config_dir: PathBuf::from("/tmp/w58/config"),
            port: 0,
            network: NetworkMode::Offline,
            frontend_dir: None,
            dev_memory_secrets: true,
            selftest_report: selftest.then(|| PathBuf::from("/tmp/w58/r.jsonl")),
            hidden,
        }
    }

    // Hidden and selftest runs neither read nor write window
    // state; a shown run keeps it in its own config dir.
    #[test]
    fn state_file_only_for_shown_runs() {
        assert_eq!(
            state_file(&opts(false, false)),
            Some(PathBuf::from("/tmp/w58/config/window-state.json"))
        );
        assert_eq!(state_file(&opts(true, false)), None);
        assert_eq!(state_file(&opts(false, true)), None);
        assert_eq!(state_file(&opts(true, true)), None);
    }

    const MAIN: Rect = Rect {
        x: 0,
        y: 0,
        w: 2560,
        h: 1440,
    };
    // A second display to the right of the main one.
    const RIGHT: Rect = Rect {
        x: 2560,
        y: 0,
        w: 1920,
        h: 1080,
    };

    fn win(x: i64, y: i64) -> Rect {
        Rect {
            x,
            y,
            w: 1280,
            h: 860,
        }
    }

    // A position saved on a display that is no longer attached
    // does not leave the window off-screen.
    #[test]
    fn off_screen_positions_are_recentred() {
        // On the main display, or on the second while it is attached.
        assert!(!needs_recentring(win(100, 100), &[MAIN]));
        assert!(!needs_recentring(win(3000, 100), &[MAIN, RIGHT]));
        // Saved on the second display, which is gone.
        assert!(needs_recentring(win(3000, 100), &[MAIN]));
        // Only a bottom corner on screen: the plugin would restore it (one
        // corner intersects), but the title bar is above the display.
        assert!(needs_recentring(win(2000, -840), &[MAIN]));
        // Title bar strip mostly off the right edge: under 120 px visible.
        assert!(needs_recentring(win(2500, 100), &[MAIN]));
        assert!(!needs_recentring(win(2400, 100), &[MAIN]));
        // Spanning two displays with the title bar on either is fine.
        assert!(!needs_recentring(win(2000, 100), &[MAIN, RIGHT]));
        // Negative coordinates (a display left of the main one).
        let left = Rect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert!(!needs_recentring(win(-1500, 50), &[MAIN, left]));
        assert!(needs_recentring(win(-1500, 50), &[MAIN]));
        // No display reported: leave it alone.
        assert!(!needs_recentring(win(99999, 99999), &[]));
    }
}
