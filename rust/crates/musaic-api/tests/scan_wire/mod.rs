//! The scan's wire form for the tests that moved here from `musaic-services`
//! when the rendering moved to `musaic_api::render::library`: the services
//! buffer's typed events and states, rendered as the HTTP face renders them.
#![allow(dead_code)]

use musaic_api::render::library as render;
use musaic_app::ops::scan::ScanEvent as OpEvent;
use musaic_services::{Json, ScanEvent, ScanState};

fn op(e: &ScanEvent) -> OpEvent {
    OpEvent::from(e.kind.clone())
}

/// The event's name on the stream.
pub fn name(e: &ScanEvent) -> &'static str {
    render::scan_event_name(&op(e))
}

/// The event's payload on the stream.
pub fn data(e: &ScanEvent) -> Json {
    render::scan_event_data(&op(e))
}

/// `GET /api/scan/status`.
pub fn state(s: &ScanState) -> Json {
    render::scan_state(s)
}
