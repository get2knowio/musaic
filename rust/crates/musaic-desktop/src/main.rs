//! `musaic-desktop` binary; see the library docs (src/lib.rs) and
//! docs/desktop.md.
// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    std::process::exit(musaic_desktop::run());
}
