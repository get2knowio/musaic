//! Event types (`musaic/library_events.py`) and their relay payload
//! (`musaic/services/library_events_relay.py::_event_to_payload` +
//! `services/sse.py::sse_event`).
//!
//! Python models events as a frozen-dataclass hierarchy:
//!
//! ```text
//! Event ─┬─ AlbumScopedEvent ─┬─ AlbumTagsChanged(album_id, fields)
//!        │                    ├─ AlbumCoverChanged(album_id)
//!        │                    ├─ AlbumMbAnchorSet(album_id, source)
//!        │                    ├─ TrackFingerprinted(album_id, filenames)
//!        │                    ├─ AlbumDeleted(album_id)
//!        │                    └─ AlbumMarkedMissing(album_id, missing_since)
//!        ├─ AlbumRenamed(old_album_id, new_album_id)
//!        ├─ LibraryScanCompleted(album_count)
//!        └─ BulkBatchCompleted()
//! ```
//!
//! Here the concrete classes are one enum ([`Event`]); the two abstract
//! classes become [`Topic::Event`] / [`Topic::AlbumScoped`], and
//! [`EventKind::mro`] reproduces the method resolution order the bus walks.

use std::fmt;

use musaic_core::pycompat::json_string;

/// One published library event. Variant and field order follow the
/// dataclass declarations; inherited `album_id` comes first, as in
/// `dataclasses.fields()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    AlbumTagsChanged {
        album_id: String,
        fields: Vec<String>,
    },
    AlbumCoverChanged {
        album_id: String,
    },
    AlbumRenamed {
        old_album_id: String,
        new_album_id: String,
    },
    AlbumMbAnchorSet {
        album_id: String,
        source: String,
    },
    TrackFingerprinted {
        album_id: String,
        filenames: Vec<String>,
    },
    AlbumDeleted {
        album_id: String,
    },
    AlbumMarkedMissing {
        album_id: String,
        missing_since: String,
    },
    LibraryScanCompleted {
        album_count: i64,
    },
    BulkBatchCompleted,
}

/// The concrete event class, without its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventKind {
    AlbumTagsChanged,
    AlbumCoverChanged,
    AlbumRenamed,
    AlbumMbAnchorSet,
    TrackFingerprinted,
    AlbumDeleted,
    AlbumMarkedMissing,
    LibraryScanCompleted,
    BulkBatchCompleted,
}

/// What a subscriber registers for: one concrete class, or one of the two
/// abstract bases (`Event` = every event, `AlbumScopedEvent` = every event
/// that carries a single `album_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Topic {
    Kind(EventKind),
    AlbumScoped,
    Event,
}

impl EventKind {
    pub const ALL: [EventKind; 9] = [
        EventKind::AlbumTagsChanged,
        EventKind::AlbumCoverChanged,
        EventKind::AlbumRenamed,
        EventKind::AlbumMbAnchorSet,
        EventKind::TrackFingerprinted,
        EventKind::AlbumDeleted,
        EventKind::AlbumMarkedMissing,
        EventKind::LibraryScanCompleted,
        EventKind::BulkBatchCompleted,
    ];

    /// The Python class name: the SSE `event:` name.
    pub fn name(self) -> &'static str {
        match self {
            EventKind::AlbumTagsChanged => "AlbumTagsChanged",
            EventKind::AlbumCoverChanged => "AlbumCoverChanged",
            EventKind::AlbumRenamed => "AlbumRenamed",
            EventKind::AlbumMbAnchorSet => "AlbumMbAnchorSet",
            EventKind::TrackFingerprinted => "TrackFingerprinted",
            EventKind::AlbumDeleted => "AlbumDeleted",
            EventKind::AlbumMarkedMissing => "AlbumMarkedMissing",
            EventKind::LibraryScanCompleted => "LibraryScanCompleted",
            EventKind::BulkBatchCompleted => "BulkBatchCompleted",
        }
    }

    pub fn from_name(name: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// Subclass of `AlbumScopedEvent`?
    pub fn is_album_scoped(self) -> bool {
        !matches!(
            self,
            EventKind::AlbumRenamed
                | EventKind::LibraryScanCompleted
                | EventKind::BulkBatchCompleted
        )
    }

    /// `type(ev).__mro__` minus `object`: the order `publish()` collects
    /// registrations in before its stable priority sort, so on equal
    /// priority a subclass subscriber runs before a base-class one.
    pub fn mro(self) -> &'static [Topic] {
        macro_rules! scoped {
            ($k:ident) => {
                &[Topic::Kind(EventKind::$k), Topic::AlbumScoped, Topic::Event]
            };
        }
        macro_rules! plain {
            ($k:ident) => {
                &[Topic::Kind(EventKind::$k), Topic::Event]
            };
        }
        match self {
            EventKind::AlbumTagsChanged => scoped!(AlbumTagsChanged),
            EventKind::AlbumCoverChanged => scoped!(AlbumCoverChanged),
            EventKind::AlbumMbAnchorSet => scoped!(AlbumMbAnchorSet),
            EventKind::TrackFingerprinted => scoped!(TrackFingerprinted),
            EventKind::AlbumDeleted => scoped!(AlbumDeleted),
            EventKind::AlbumMarkedMissing => scoped!(AlbumMarkedMissing),
            EventKind::AlbumRenamed => plain!(AlbumRenamed),
            EventKind::LibraryScanCompleted => plain!(LibraryScanCompleted),
            EventKind::BulkBatchCompleted => plain!(BulkBatchCompleted),
        }
    }
}

impl Topic {
    /// The Python class name the topic stands for.
    pub fn name(self) -> &'static str {
        match self {
            Topic::Kind(k) => k.name(),
            Topic::AlbumScoped => "AlbumScopedEvent",
            Topic::Event => "Event",
        }
    }

    pub fn from_name(name: &str) -> Option<Topic> {
        match name {
            "Event" => Some(Topic::Event),
            "AlbumScopedEvent" => Some(Topic::AlbumScoped),
            _ => EventKind::from_name(name).map(Topic::Kind),
        }
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One dataclass field value. Every event field is a `str`, an `int` or a
/// `tuple[str, ...]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldValue<'a> {
    Str(&'a str),
    Int(i64),
    StrList(&'a [String]),
}

/// Why [`Event::from_fields`] refused its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromFieldsError {
    UnknownEvent(String),
    /// Python would raise `TypeError` (missing required argument).
    Missing(&'static str),
    /// Python would raise `TypeError` (unexpected keyword argument).
    Unexpected(String),
    /// The value is not the declared type. Includes `None`: Python does
    /// not check dataclass field types, Rust does (see the crate docs).
    WrongType(&'static str),
}

impl fmt::Display for FromFieldsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FromFieldsError::UnknownEvent(n) => write!(f, "unknown event class {n:?}"),
            FromFieldsError::Missing(n) => write!(f, "missing required field {n:?}"),
            FromFieldsError::Unexpected(n) => write!(f, "unexpected field {n:?}"),
            FromFieldsError::WrongType(n) => write!(f, "field {n:?} has the wrong type"),
        }
    }
}

impl std::error::Error for FromFieldsError {}

/// A field value handed to [`Event::from_fields`]; mirrors the kwargs a
/// Python producer passes. `Null` exists only so callers can express
/// Python's `None` and see it refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgValue {
    Null,
    Str(String),
    Int(i64),
    StrList(Vec<String>),
    /// Anything else (a float, a mixed list, a mapping...).
    Other,
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Event::AlbumTagsChanged { .. } => EventKind::AlbumTagsChanged,
            Event::AlbumCoverChanged { .. } => EventKind::AlbumCoverChanged,
            Event::AlbumRenamed { .. } => EventKind::AlbumRenamed,
            Event::AlbumMbAnchorSet { .. } => EventKind::AlbumMbAnchorSet,
            Event::TrackFingerprinted { .. } => EventKind::TrackFingerprinted,
            Event::AlbumDeleted { .. } => EventKind::AlbumDeleted,
            Event::AlbumMarkedMissing { .. } => EventKind::AlbumMarkedMissing,
            Event::LibraryScanCompleted { .. } => EventKind::LibraryScanCompleted,
            Event::BulkBatchCompleted => EventKind::BulkBatchCompleted,
        }
    }

    /// The SSE event name (`type(ev).__name__`).
    pub fn name(&self) -> &'static str {
        self.kind().name()
    }

    /// `ev.album_id` for `AlbumScopedEvent` subclasses.
    pub fn album_id(&self) -> Option<&str> {
        match self {
            Event::AlbumTagsChanged { album_id, .. }
            | Event::AlbumCoverChanged { album_id }
            | Event::AlbumMbAnchorSet { album_id, .. }
            | Event::TrackFingerprinted { album_id, .. }
            | Event::AlbumDeleted { album_id }
            | Event::AlbumMarkedMissing { album_id, .. } => Some(album_id),
            _ => None,
        }
    }

    /// `dataclasses.fields(ev)` with values, in declaration order.
    pub fn fields(&self) -> Vec<(&'static str, FieldValue<'_>)> {
        use FieldValue::*;
        match self {
            Event::AlbumTagsChanged { album_id, fields } => {
                vec![("album_id", Str(album_id)), ("fields", StrList(fields))]
            }
            Event::AlbumCoverChanged { album_id } | Event::AlbumDeleted { album_id } => {
                vec![("album_id", Str(album_id))]
            }
            Event::AlbumRenamed {
                old_album_id,
                new_album_id,
            } => vec![
                ("old_album_id", Str(old_album_id)),
                ("new_album_id", Str(new_album_id)),
            ],
            Event::AlbumMbAnchorSet { album_id, source } => {
                vec![("album_id", Str(album_id)), ("source", Str(source))]
            }
            Event::TrackFingerprinted {
                album_id,
                filenames,
            } => vec![
                ("album_id", Str(album_id)),
                ("filenames", StrList(filenames)),
            ],
            Event::AlbumMarkedMissing {
                album_id,
                missing_since,
            } => vec![
                ("album_id", Str(album_id)),
                ("missing_since", Str(missing_since)),
            ],
            Event::LibraryScanCompleted { album_count } => {
                vec![("album_count", Int(*album_count))]
            }
            Event::BulkBatchCompleted => vec![],
        }
    }

    /// The relay's SSE `data:` line: `json.dumps(payload)` with Python's
    /// defaults (`", "` / `": "` separators, `ensure_ascii=True`), keys in
    /// field order, tuples as lists.
    pub fn payload_json(&self) -> String {
        let mut out = String::from("{");
        for (i, (name, value)) in self.fields().into_iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&json_string(name));
            out.push_str(": ");
            match value {
                FieldValue::Str(s) => out.push_str(&json_string(s)),
                FieldValue::Int(n) => out.push_str(&n.to_string()),
                FieldValue::StrList(items) => {
                    out.push('[');
                    for (j, s) in items.iter().enumerate() {
                        if j > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&json_string(s));
                    }
                    out.push(']');
                }
            }
        }
        out.push('}');
        out
    }

    /// Build an event from its class name and keyword arguments, applying the
    /// dataclass defaults (`fields=()`, `filenames=()`, `source=""`,
    /// `missing_since=""`).
    pub fn from_fields<'a>(
        class: &str,
        args: impl IntoIterator<Item = (&'a str, ArgValue)>,
    ) -> Result<Event, FromFieldsError> {
        let kind = EventKind::from_name(class)
            .ok_or_else(|| FromFieldsError::UnknownEvent(class.to_owned()))?;
        let mut args: Vec<(&str, ArgValue)> = args.into_iter().collect();
        let mut take = |name: &'static str| -> Option<ArgValue> {
            let i = args.iter().position(|(k, _)| *k == name)?;
            Some(args.remove(i).1)
        };
        fn s(name: &'static str, v: Option<ArgValue>) -> Result<String, FromFieldsError> {
            match v {
                Some(ArgValue::Str(x)) => Ok(x),
                None => Err(FromFieldsError::Missing(name)),
                Some(_) => Err(FromFieldsError::WrongType(name)),
            }
        }
        fn s_or(name: &'static str, v: Option<ArgValue>) -> Result<String, FromFieldsError> {
            match v {
                None => Ok(String::new()),
                v => s(name, v),
            }
        }
        fn list_or(
            name: &'static str,
            v: Option<ArgValue>,
        ) -> Result<Vec<String>, FromFieldsError> {
            match v {
                None => Ok(Vec::new()),
                Some(ArgValue::StrList(x)) => Ok(x),
                Some(_) => Err(FromFieldsError::WrongType(name)),
            }
        }
        let ev = match kind {
            EventKind::AlbumTagsChanged => Event::AlbumTagsChanged {
                album_id: s("album_id", take("album_id"))?,
                fields: list_or("fields", take("fields"))?,
            },
            EventKind::AlbumCoverChanged => Event::AlbumCoverChanged {
                album_id: s("album_id", take("album_id"))?,
            },
            EventKind::AlbumRenamed => Event::AlbumRenamed {
                old_album_id: s("old_album_id", take("old_album_id"))?,
                new_album_id: s("new_album_id", take("new_album_id"))?,
            },
            EventKind::AlbumMbAnchorSet => Event::AlbumMbAnchorSet {
                album_id: s("album_id", take("album_id"))?,
                source: s_or("source", take("source"))?,
            },
            EventKind::TrackFingerprinted => Event::TrackFingerprinted {
                album_id: s("album_id", take("album_id"))?,
                filenames: list_or("filenames", take("filenames"))?,
            },
            EventKind::AlbumDeleted => Event::AlbumDeleted {
                album_id: s("album_id", take("album_id"))?,
            },
            EventKind::AlbumMarkedMissing => Event::AlbumMarkedMissing {
                album_id: s("album_id", take("album_id"))?,
                missing_since: s_or("missing_since", take("missing_since"))?,
            },
            EventKind::LibraryScanCompleted => match take("album_count") {
                Some(ArgValue::Int(n)) => Event::LibraryScanCompleted { album_count: n },
                None => return Err(FromFieldsError::Missing("album_count")),
                Some(_) => return Err(FromFieldsError::WrongType("album_count")),
            },
            EventKind::BulkBatchCompleted => Event::BulkBatchCompleted,
        };
        if let Some((k, _)) = args.first() {
            return Err(FromFieldsError::Unexpected((*k).to_owned()));
        }
        Ok(ev)
    }
}

/// One SSE frame as `services/sse.py::sse_event` writes it.
pub fn sse_frame(name: &str, data_json: &str) -> String {
    format!("event: {name}\ndata: {data_json}\n\n")
}

/// The handshake the `/api/library-events/stream` handler yields first
/// (`sse_event("ready", {})`).
pub fn ready_frame() -> String {
    sse_frame("ready", "{}")
}
